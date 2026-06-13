// monado-frame — in-headset overlay for Monado.
//
// Overlay session (XR_EXTX_overlay) rendering two grabbable egui panels — a
// gesture settings panel and a screenshot-review panel — plus a controller
// laser pointer. See config.rs / shots.rs / mathx.rs for the split-out bits.

mod config;
mod mathx;
mod shots;

use std::env;
use std::fs;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use anyhow::{bail, Result};
use ash::vk;
use ash::vk::Handle as _;
use openxr as xr;

use config::Settings;
use mathx::{cross, dot, forward, locate_pose, normalize, pose_compose, pose_invert, q_mul, qf, quat_from_axes, quat_from_euler_deg, quatf, raycast, vec3f};
use shots::{Photo, PhotoAction};

static VK_ENTRY: OnceLock<ash::Entry> = OnceLock::new();
static RENDER_PASS: OnceLock<vk::RenderPass> = OnceLock::new();

const PPP: f32 = 1.5;
const GRAB_START: f32 = 0.40; // grip FORCE to start grabbing
const GRAB_RELEASE: f32 = 0.15;
const IDENTITY_QUAT: xr::Quaternionf = xr::Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 };

unsafe extern "system" fn get_instance_proc_addr(
    instance: xr::sys::platform::VkInstance,
    name: *const c_char,
) -> Option<unsafe extern "system" fn()> {
    let entry = VK_ENTRY.get().expect("vk entry not initialised");
    let vk_instance = vk::Instance::from_raw(instance as _);
    (entry.static_fn().get_instance_proc_addr)(vk_instance, name)
}

mod theme {
    use egui::Color32;
    pub const PRIMARY: Color32 = Color32::from_rgb(160, 200, 255);
    pub const SURFACE: Color32 = Color32::from_rgb(19, 19, 24);
    pub const SURFACE_CONTAINER: Color32 = Color32::from_rgb(32, 31, 39);
    pub const SURFACE_CONTAINER_HIGH: Color32 = Color32::from_rgb(43, 42, 51);
    pub const ON_SURFACE: Color32 = Color32::from_rgb(230, 225, 233);
    pub const ON_SURFACE_VAR: Color32 = Color32::from_rgb(196, 199, 209);
}

// ---------------------------------------------------------------------------
// Styling + UI
// ---------------------------------------------------------------------------

fn apply_style(ctx: &egui::Context) {
    use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle};
    let mut style = (*ctx.style()).clone();
    let mut v = egui::Visuals::dark();
    v.panel_fill = theme::SURFACE;
    v.window_fill = theme::SURFACE_CONTAINER;
    v.faint_bg_color = theme::SURFACE_CONTAINER;
    v.extreme_bg_color = Color32::from_rgb(14, 14, 18);
    v.override_text_color = Some(theme::ON_SURFACE);
    v.selection.bg_fill = Color32::from_rgb(48, 78, 130);
    v.selection.stroke = Stroke::new(1.0, theme::PRIMARY);
    v.hyperlink_color = theme::PRIMARY;
    v.widgets.noninteractive.bg_fill = theme::SURFACE;
    v.widgets.inactive.bg_fill = theme::SURFACE_CONTAINER_HIGH;
    v.widgets.inactive.weak_bg_fill = theme::SURFACE_CONTAINER_HIGH;
    v.widgets.hovered.bg_fill = Color32::from_rgb(56, 64, 82);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(56, 64, 82);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    v.widgets.active.bg_fill = theme::PRIMARY;
    v.widgets.active.weak_bg_fill = theme::PRIMARY;
    v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::BLACK);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::same(16);
        w.bg_stroke = Stroke::NONE;
    }
    style.visuals = v;
    style.spacing.item_spacing = egui::vec2(10.0, 12.0);
    style.spacing.button_padding = egui::vec2(16.0, 10.0);
    style.spacing.slider_width = 220.0;
    style.spacing.interact_size.y = 30.0;
    style.text_styles.insert(TextStyle::Heading, FontId::new(24.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Body, FontId::new(17.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Button, FontId::new(17.0, FontFamily::Proportional));
    style.text_styles.insert(TextStyle::Small, FontId::new(12.0, FontFamily::Proportional));
    ctx.set_style(style);
}

// The rounded, (optionally) translucent floating panel surface. With a
// transparent framebuffer + an alpha-blended quad layer this gives rounded
// outer corners and glass.
fn panel_card<R>(ui: &mut egui::Ui, alpha: u8, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let s = theme::SURFACE;
    egui::Frame::default()
        .fill(egui::Color32::from_rgba_unmultiplied(s.r(), s.g(), s.b(), alpha))
        .corner_radius(16)
        .outer_margin(10)
        .inner_margin(18)
        .shadow(egui::Shadow { offset: [0, 6], blur: 22, spread: 0, color: egui::Color32::from_black_alpha(120) })
        .show(ui, add)
        .inner
}

fn header(ui: &mut egui::Ui, left: String, right: Option<String>) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(left).size(20.0).strong().color(egui::Color32::WHITE));
        if let Some(r) = right {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(r).color(theme::ON_SURFACE_VAR));
            });
        }
    });
    ui.add_space(6.0);
    ui.separator();
    ui.add_space(8.0);
}

fn paint_corner_brackets(painter: &egui::Painter, r: egui::Rect, len: f32, stroke: egui::Stroke) {
    use egui::vec2;
    painter.line_segment([r.left_top(), r.left_top() + vec2(len, 0.0)], stroke);
    painter.line_segment([r.left_top(), r.left_top() + vec2(0.0, len)], stroke);
    painter.line_segment([r.right_top(), r.right_top() + vec2(-len, 0.0)], stroke);
    painter.line_segment([r.right_top(), r.right_top() + vec2(0.0, len)], stroke);
    painter.line_segment([r.left_bottom(), r.left_bottom() + vec2(len, 0.0)], stroke);
    painter.line_segment([r.left_bottom(), r.left_bottom() + vec2(0.0, -len)], stroke);
    painter.line_segment([r.right_bottom(), r.right_bottom() + vec2(-len, 0.0)], stroke);
    painter.line_segment([r.right_bottom(), r.right_bottom() + vec2(0.0, -len)], stroke);
}

fn build_settings(ctx: &egui::Context, s: &mut Settings, changed: &mut bool, open_gallery: &mut bool, alpha: u8) {
    use egui_phosphor::regular as icons;
    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
        panel_card(ui, alpha, |ui| {
            header(ui, format!("{}  monado-frame", icons::GEAR_SIX), None);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("Finger-frame gesture").color(theme::ON_SURFACE_VAR));
            });
            ui.add_space(12.0);
            *changed |= ui.checkbox(&mut s.enabled, "Gesture enabled").changed();
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Hold delay").color(theme::ON_SURFACE_VAR));
            ui.add_space(4.0);
            *changed |= ui.add(egui::Slider::new(&mut s.hold_ms, 500..=4000).suffix(" ms")).changed();
            ui.add_space(16.0);
            ui.separator();
            ui.add_space(10.0);
            ui.vertical_centered(|ui| {
                let label = egui::RichText::new(format!("{}  Open gallery", icons::IMAGES)).color(egui::Color32::BLACK);
                if ui.add(egui::Button::new(label).fill(theme::PRIMARY)).clicked() {
                    *open_gallery = true;
                }
                ui.add_space(8.0);
                ui.small(egui::RichText::new(&s.path).color(theme::ON_SURFACE_VAR));
            });
        });
    });
}

fn build_photo(ctx: &egui::Context, tex: Option<&egui::TextureHandle>, when: &str, action: &mut PhotoAction, alpha: u8) {
    use egui_phosphor::regular as icons;
    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
        panel_card(ui, alpha, |ui| {
            header(ui, format!("{}  Screenshot", icons::CAMERA), Some(when.to_string()));

            // Reserve the action row at the bottom; the image fills the rest.
            let footer_h = 48.0;
            let body_h = (ui.available_height() - footer_h).max(40.0);
            ui.allocate_ui(egui::vec2(ui.available_width(), body_h), |ui| {
                ui.centered_and_justified(|ui| {
                    if let Some(t) = tex {
                        let resp = ui.add(egui::Image::new(t).max_size(ui.available_size() * 0.96).corner_radius(8));
                        paint_corner_brackets(ui.painter(), resp.rect.expand(8.0), 24.0, egui::Stroke::new(2.5, theme::PRIMARY));
                    } else {
                        ui.label(egui::RichText::new("No screenshot").color(theme::ON_SURFACE_VAR));
                    }
                });
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(format!("{}  Copy", icons::COPY)).clicked() {
                    *action = PhotoAction::Copy;
                }
                if ui.button(format!("{}  Delete", icons::TRASH)).clicked() {
                    *action = PhotoAction::Delete;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(format!("{}  Close", icons::X)).clicked() {
                        *action = PhotoAction::Dismiss;
                    }
                });
            });
        });
    });
}

// ---------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------

// Everything the controller can point at. Photo windows are pooled, addressed
// by slot index.
#[derive(Clone, Copy, PartialEq)]
enum Target {
    Settings,
    Gallery,
    Wrist,
    Photo(usize),
}

enum WristAction {
    None,
    Older,
    Newer,
    Open,
}

enum GalleryAction {
    None,
    Close,
    Open(usize),
    PrevPage,
    NextPage,
}

fn parse3(s: &str) -> Option<[f32; 3]> {
    let mut it = s.split(',').map(|x| x.trim().parse::<f32>());
    let a = it.next()?.ok()?;
    let b = it.next()?.ok()?;
    let c = it.next()?.ok()?;
    if it.next().is_some() {
        return None;
    }
    Some([a, b, c])
}

// The wrist notification card: mini preview + date, with ‹ › to scroll the
// pending queue. Clicking the preview opens that shot as a floating panel.
#[allow(clippy::too_many_arguments)]
fn build_wrist(
    ctx: &egui::Context,
    thumb: Option<&egui::TextureHandle>,
    when: &str,
    idx: usize,
    total: usize,
    alpha: u8,
) -> WristAction {
    use egui_phosphor::regular as icons;
    let mut action = WristAction::None;
    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
        panel_card(ui, alpha, |ui| {
            ui.horizontal(|ui| {
                let size = egui::vec2(100.0, 72.0);
                if let Some(t) = thumb {
                    let img = egui::Image::new(t).fit_to_exact_size(size).corner_radius(8);
                    if ui.add(egui::ImageButton::new(img).frame(false)).on_hover_text("Open").clicked() {
                        action = WristAction::Open;
                    }
                } else {
                    ui.allocate_space(size);
                }
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(format!("{}  New screenshot", icons::CAMERA)).strong().color(egui::Color32::WHITE));
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new(when).size(15.0).color(theme::ON_SURFACE_VAR));
                });
            });
            if total > 1 {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let older = egui::Button::new(egui::RichText::new(icons::CARET_LEFT).size(18.0));
                    if ui.add_enabled(idx + 1 < total, older).on_hover_text("Older").clicked() {
                        action = WristAction::Older;
                    }
                    ui.label(egui::RichText::new(format!("{} / {}", idx + 1, total)).color(theme::ON_SURFACE_VAR));
                    let newer = egui::Button::new(egui::RichText::new(icons::CARET_RIGHT).size(18.0));
                    if ui.add_enabled(idx > 0, newer).on_hover_text("Newer").clicked() {
                        action = WristAction::Newer;
                    }
                });
            }
        });
    });
    action
}

// The dedicated gallery: a paged grid of (already-decoded) page thumbnails.
// Clicking one opens it as a floating photo panel. Paged (not scrolled) so it's
// all raycast-clickable. `total` is the full screenshot count across all pages.
#[allow(clippy::too_many_arguments)]
fn build_gallery(
    ctx: &egui::Context,
    items: &[(egui::TextureHandle, String)],
    page: usize,
    total: usize,
    action: &mut GalleryAction,
    alpha: u8,
) {
    use egui_phosphor::regular as icons;
    const COLS: usize = 4;
    let pages = total.div_ceil(GALLERY_PER).max(1);
    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ctx, |ui| {
        panel_card(ui, alpha, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("{}  Gallery", icons::IMAGES)).size(20.0).strong().color(egui::Color32::WHITE));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(format!("{}  Close", icons::X)).clicked() {
                        *action = GalleryAction::Close;
                    }
                    ui.label(egui::RichText::new(format!("{total} shots")).color(theme::ON_SURFACE_VAR));
                });
            });
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(8.0);
            if total == 0 {
                ui.vertical_centered(|ui| ui.label(egui::RichText::new("No screenshots yet").color(theme::ON_SURFACE_VAR)));
                return;
            }
            egui::Grid::new("gallery_grid").spacing(egui::vec2(14.0, 14.0)).show(ui, |ui| {
                for (k, (tex, when)) in items.iter().enumerate() {
                    ui.vertical(|ui| {
                        let img = egui::Image::new(tex).fit_to_exact_size(egui::vec2(150.0, 112.0)).corner_radius(8);
                        if ui.add(egui::ImageButton::new(img).frame(false)).clicked() {
                            *action = GalleryAction::Open(k);
                        }
                        ui.small(egui::RichText::new(when).color(theme::ON_SURFACE_VAR));
                    });
                    if (k + 1) % COLS == 0 {
                        ui.end_row();
                    }
                }
            });
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let prev = egui::Button::new(egui::RichText::new(icons::CARET_LEFT).size(18.0));
                if ui.add_enabled(page > 0, prev).clicked() {
                    *action = GalleryAction::PrevPage;
                }
                ui.label(egui::RichText::new(format!("Page {} / {}", page + 1, pages)).color(theme::ON_SURFACE_VAR));
                let next = egui::Button::new(egui::RichText::new(icons::CARET_RIGHT).size(18.0));
                if ui.add_enabled(page + 1 < pages, next).clicked() {
                    *action = GalleryAction::NextPage;
                }
            });
        });
    });
}

// A queued screenshot notification shown on the wrist (mini preview + date).
struct Pending {
    path: PathBuf,
    when: String,
    thumb: egui::TextureHandle,
}

// A pooled floating photo window. `open` toggles whether it's shown/interactive.
struct PhotoSlot {
    gfx: PanelGfx,
    open: bool,
    photo: Option<Photo>,
    path: Option<PathBuf>,
    when: String,
}

fn close_slot(s: &mut PhotoSlot) {
    s.open = false;
    s.photo = None;
    s.path = None;
    s.when.clear();
}

// Open `path` in a free pool slot (reusing slot 0 if all are taken), positioned
// in front of the head and offset per slot so multiple windows don't stack.
fn open_photo(pool: &mut [PhotoSlot], path: &Path, when: &str, hmd: Option<xr::Posef>) {
    let slot = pool.iter().position(|s| !s.open).unwrap_or(0);
    match shots::load(&pool[slot].gfx.ctx, path) {
        Ok(p) => {
            let s = &mut pool[slot];
            s.photo = Some(p);
            s.path = Some(path.to_path_buf());
            s.when = when.to_string();
            s.open = true;
            if let Some(h) = hmd {
                s.gfx.pose = front_pose(&h, 0.9, (slot as f32 - 1.0) * 0.34);
            }
            log::info!("opened photo in slot {slot}: {}", path.display());
        }
        Err(e) => log::warn!("open photo {path:?}: {e}"),
    }
}

const GALLERY_PER: usize = 12; // thumbnails per gallery page

// All screenshots as (path, date), newest-first. Cheap (no image decode).
fn gallery_scan(dir: &str) -> Vec<(PathBuf, String)> {
    shots::scan_all(dir).into_iter().map(|(p, _)| { let w = shots::shot_time(&p); (p, w) }).collect()
}

// Decode just one page of thumbnails into `ctx` (keeps the open/page-turn hitch
// bounded instead of decoding every screenshot up front).
fn gallery_page_items(ctx: &egui::Context, paths: &[(PathBuf, String)], page: usize) -> Vec<(egui::TextureHandle, String)> {
    let start = page * GALLERY_PER;
    let end = (start + GALLERY_PER).min(paths.len());
    let mut out = Vec::new();
    for (path, when) in &paths[start.min(paths.len())..end] {
        match shots::load_thumb(ctx, path, 256) {
            Ok(t) => out.push((t, when.clone())),
            Err(e) => log::warn!("gallery thumb {path:?}: {e}"),
        }
    }
    out
}

// A panel pose `dist` metres ahead of the head (and `lateral` to the side),
// upright and facing the user. Recomputed each time a panel is opened.
fn front_pose(h: &xr::Posef, dist: f32, lateral: f32) -> xr::Posef {
    let fwd = normalize(forward(h));
    let up = [0.0, 1.0, 0.0];
    let right = normalize(cross(fwd, up));
    let o = [h.position.x, h.position.y, h.position.z];
    let pos = [
        o[0] + fwd[0] * dist + right[0] * lateral,
        o[1] + fwd[1] * dist + right[1] * lateral,
        o[2] + fwd[2] * dist + right[2] * lateral,
    ];
    let z = normalize([o[0] - pos[0], o[1] - pos[1], o[2] - pos[2]]); // face the head
    let x = normalize(cross(up, z));
    let y = cross(z, x);
    xr::Posef { orientation: quatf(quat_from_axes(x, y, z)), position: vec3f(pos) }
}

struct PanelGfx {
    swapchain: xr::Swapchain<xr::Vulkan>,
    framebuffers: Vec<vk::Framebuffer>,
    ctx: egui::Context,
    renderer: egui_ash_renderer::Renderer,
    px: (u32, u32),
    size_m: (f32, f32),
    pose: xr::Posef,
    prev_pos: Option<egui::Pos2>,
    prev_down: bool,
}

#[allow(clippy::too_many_arguments)]
fn make_panel(
    session: &xr::Session<xr::Vulkan>,
    device: &ash::Device,
    allocator: Arc<Mutex<gpu_allocator::vulkan::Allocator>>,
    render_pass: vk::RenderPass,
    format: vk::Format,
    srgb: bool,
    px: (u32, u32),
    size_m: (f32, f32),
    pose: xr::Posef,
) -> Result<PanelGfx> {
    let swapchain = session.create_swapchain(&xr::SwapchainCreateInfo {
        create_flags: xr::SwapchainCreateFlags::EMPTY,
        usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT,
        format: format.as_raw() as _,
        sample_count: 1,
        width: px.0,
        height: px.1,
        face_count: 1,
        array_size: 1,
        mip_count: 1,
    })?;
    let images: Vec<vk::Image> = swapchain.enumerate_images()?.into_iter().map(vk::Image::from_raw).collect();
    let framebuffers = make_framebuffers(device, render_pass, format, &images, px)?;

    let ctx = egui::Context::default();
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);
    apply_style(&ctx);
    ctx.set_pixels_per_point(PPP);
    ctx.options_mut(|o| {
        o.input_options.max_click_dist = 80.0;
        o.input_options.max_click_duration = 3.0;
    });

    let renderer = egui_ash_renderer::Renderer::with_gpu_allocator(
        allocator,
        device.clone(),
        render_pass,
        egui_ash_renderer::Options { srgb_framebuffer: srgb, ..Default::default() },
    )
    .map_err(|e| anyhow::anyhow!("egui renderer init: {e}"))?;

    Ok(PanelGfx { swapchain, framebuffers, ctx, renderer, px, size_m, pose, prev_pos: None, prev_down: false })
}

fn make_framebuffers(
    device: &ash::Device,
    render_pass: vk::RenderPass,
    format: vk::Format,
    images: &[vk::Image],
    px: (u32, u32),
) -> Result<Vec<vk::Framebuffer>> {
    let range = vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    };
    let mut fbs = Vec::with_capacity(images.len());
    for &img in images {
        let view = unsafe {
            device.create_image_view(
                &vk::ImageViewCreateInfo::default()
                    .image(img)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(range),
                None,
            )?
        };
        let atts = [view];
        let fb = unsafe {
            device.create_framebuffer(
                &vk::FramebufferCreateInfo::default()
                    .render_pass(render_pass)
                    .attachments(&atts)
                    .width(px.0)
                    .height(px.1)
                    .layers(1),
                None,
            )?
        };
        fbs.push(fb);
    }
    Ok(fbs)
}

#[allow(clippy::too_many_arguments)]
fn render_panel(
    p: &mut PanelGfx,
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    cmd_pool: vk::CommandPool,
    queue: vk::Queue,
    fence: vk::Fence,
    alpha_mode: bool,
    pointer: Option<(f32, f32, bool)>,
    mut build: impl FnMut(&egui::Context),
) -> Result<()> {
    let pos = pointer.map(|(u, v, _)| egui::pos2(u * p.px.0 as f32 / PPP, v * p.px.1 as f32 / PPP));
    let down = pointer.is_some_and(|(_, _, d)| d);

    let mut events = Vec::new();
    if let Some(ps) = pos {
        events.push(egui::Event::PointerMoved(ps));
    } else if p.prev_pos.is_some() {
        events.push(egui::Event::PointerGone);
    }
    if down != p.prev_down {
        if let Some(ps) = pos.or(p.prev_pos) {
            events.push(egui::Event::PointerButton {
                pos: ps,
                button: egui::PointerButton::Primary,
                pressed: down,
                modifiers: egui::Modifiers::default(),
            });
        }
    }
    p.prev_pos = pos;
    p.prev_down = down;

    let raw = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(p.px.0 as f32 / PPP, p.px.1 as f32 / PPP),
        )),
        events,
        ..Default::default()
    };
    let out = p.ctx.run(raw, |ctx| {
        build(ctx);
        if let Some(ps) = pos {
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("cursor")));
            painter.circle_filled(ps, 5.0, theme::PRIMARY);
            painter.circle_stroke(ps, 5.0, egui::Stroke::new(1.5, egui::Color32::from_black_alpha(150)));
        }
    });

    let prims = p.ctx.tessellate(out.shapes, out.pixels_per_point);
    p.renderer
        .set_textures(queue, cmd_pool, &out.textures_delta.set)
        .map_err(|e| anyhow::anyhow!("set_textures: {e}"))?;

    let index = p.swapchain.acquire_image()?;
    p.swapchain.wait_image(xr::Duration::INFINITE)?;
    let clear = if alpha_mode { [0.0, 0.0, 0.0, 0.0] } else { [0.07, 0.07, 0.09, 1.0] };
    unsafe {
        device.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())?;
        device.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;
        let clears = [vk::ClearValue { color: vk::ClearColorValue { float32: clear } }];
        let rp = vk::RenderPassBeginInfo::default()
            .render_pass(RENDER_PASS.get().copied().unwrap())
            .framebuffer(p.framebuffers[index as usize])
            .render_area(vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: vk::Extent2D { width: p.px.0, height: p.px.1 } })
            .clear_values(&clears);
        device.cmd_begin_render_pass(cmd, &rp, vk::SubpassContents::INLINE);
        p.renderer
            .cmd_draw(cmd, vk::Extent2D { width: p.px.0, height: p.px.1 }, out.pixels_per_point, &prims)
            .map_err(|e| anyhow::anyhow!("cmd_draw: {e}"))?;
        device.cmd_end_render_pass(cmd);
        device.end_command_buffer(cmd)?;
        let cmds = [cmd];
        let submit = vk::SubmitInfo::default().command_buffers(&cmds);
        device.queue_submit(queue, &[submit], fence)?;
        device.wait_for_fences(&[fence], true, u64::MAX)?;
        device.reset_fences(&[fence])?;
    }
    p.renderer.free_textures(&out.textures_delta.free).map_err(|e| anyhow::anyhow!("free_textures: {e}"))?;
    p.swapchain.release_image()?;
    Ok(())
}

fn quad_layer<'a>(p: &'a PanelGfx, space: &'a xr::Space, alpha_mode: bool) -> xr::CompositionLayerQuad<'a, xr::Vulkan> {
    let sub = xr::SwapchainSubImage::new().swapchain(&p.swapchain).image_array_index(0).image_rect(xr::Rect2Di {
        offset: xr::Offset2Di { x: 0, y: 0 },
        extent: xr::Extent2Di { width: p.px.0 as i32, height: p.px.1 as i32 },
    });
    let mut q = xr::CompositionLayerQuad::new()
        .space(space)
        .eye_visibility(xr::EyeVisibility::BOTH)
        .sub_image(sub)
        .pose(p.pose)
        .size(xr::Extent2Df { width: p.size_m.0, height: p.size_m.1 });
    if alpha_mode {
        q = q.layer_flags(xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA);
    }
    q
}

// ---------------------------------------------------------------------------
// Laser pointer
// ---------------------------------------------------------------------------

struct Laser {
    swapchain: xr::Swapchain<xr::Vulkan>,
    images: Vec<vk::Image>,
}

fn make_laser(session: &xr::Session<xr::Vulkan>, format: vk::Format) -> Result<Laser> {
    let swapchain = session.create_swapchain(&xr::SwapchainCreateInfo {
        create_flags: xr::SwapchainCreateFlags::EMPTY,
        usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT | xr::SwapchainUsageFlags::TRANSFER_DST,
        format: format.as_raw() as _,
        sample_count: 1,
        width: 8,
        height: 8,
        face_count: 1,
        array_size: 1,
        mip_count: 1,
    })?;
    let images = swapchain.enumerate_images()?.into_iter().map(vk::Image::from_raw).collect();
    Ok(Laser { swapchain, images })
}

// Fill the laser texture with the accent colour (called per frame it's shown).
// Must clear the image the runtime actually handed us, not always images[0],
// or the rotating swapchain shows uncleared images and the beam flickers.
fn fill_laser(laser: &mut Laser, device: &ash::Device, cmd: vk::CommandBuffer, queue: vk::Queue, fence: vk::Fence) -> Result<()> {
    let index = laser.swapchain.acquire_image()? as usize;
    laser.swapchain.wait_image(xr::Duration::INFINITE)?;
    let image = laser.images[index];
    let range = vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    };
    unsafe {
        device.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())?;
        device.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))?;
        let to_dst = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(range);
        device.cmd_pipeline_barrier(cmd, vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[], &[to_dst]);
        let color = vk::ClearColorValue { float32: [0.39, 0.63, 1.0, 1.0] };
        device.cmd_clear_color_image(cmd, image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &color, &[range]);
        let to_src = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(range);
        device.cmd_pipeline_barrier(cmd, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT, vk::DependencyFlags::empty(), &[], &[], &[to_src]);
        device.end_command_buffer(cmd)?;
        let cmds = [cmd];
        device.queue_submit(queue, &[vk::SubmitInfo::default().command_buffers(&cmds)], fence)?;
        device.wait_for_fences(&[fence], true, u64::MAX)?;
        device.reset_fences(&[fence])?;
    }
    laser.swapchain.release_image()?;
    Ok(())
}

// A thin quad from the controller to the hit point, billboarded toward the HMD.
fn laser_quad<'a>(laser: &'a Laser, space: &'a xr::Space, aim: &xr::Posef, dist: f32, hmd: &xr::Posef) -> xr::CompositionLayerQuad<'a, xr::Vulkan> {
    let o = [aim.position.x, aim.position.y, aim.position.z];
    let dir = normalize(forward(aim));
    let mid = [o[0] + dir[0] * dist * 0.5, o[1] + dir[1] * dist * 0.5, o[2] + dir[2] * dist * 0.5];
    let to_view = normalize([hmd.position.x - mid[0], hmd.position.y - mid[1], hmd.position.z - mid[2]]);
    let x = normalize(cross(dir, to_view));
    let z = cross(x, dir);
    let q = quat_from_axes(x, dir, z);
    let sub = xr::SwapchainSubImage::new().swapchain(&laser.swapchain).image_array_index(0).image_rect(xr::Rect2Di {
        offset: xr::Offset2Di { x: 0, y: 0 },
        extent: xr::Extent2Di { width: 8, height: 8 },
    });
    xr::CompositionLayerQuad::new()
        .space(space)
        .eye_visibility(xr::EyeVisibility::BOTH)
        .sub_image(sub)
        .pose(xr::Posef { orientation: quatf(q), position: vec3f(mid) })
        .size(xr::Extent2Df { width: 0.006, height: dist })
}

// ---------------------------------------------------------------------------

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    if let Err(e) = run() {
        log::error!("monado-frame exited with error: {e:?}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let entry = xr::Entry::linked();
    let available = entry.enumerate_extensions()?;
    if !available.khr_vulkan_enable2 {
        bail!("runtime is missing XR_KHR_vulkan_enable2");
    }
    if !available.extx_overlay {
        bail!("runtime is missing XR_EXTX_overlay");
    }
    let mut exts = xr::ExtensionSet::default();
    exts.khr_vulkan_enable2 = true;
    exts.extx_overlay = true;
    let xr_instance = entry.create_instance(
        &xr::ApplicationInfo {
            api_version: xr::Version::new(1, 0, 32),
            application_name: "monado-frame",
            application_version: 0,
            engine_name: "monado-frame",
            engine_version: 0,
        },
        &exts,
        &[],
    )?;
    let props = xr_instance.properties()?;
    log::info!("OpenXR runtime: {} {}", props.runtime_name, props.runtime_version);
    let system = xr_instance.system(xr::FormFactor::HEAD_MOUNTED_DISPLAY)?;
    let _reqs = xr_instance.graphics_requirements::<xr::Vulkan>(system)?;
    let blend_mode = xr_instance
        .enumerate_environment_blend_modes(system, xr::ViewConfigurationType::PRIMARY_STEREO)?
        .first()
        .copied()
        .unwrap_or(xr::EnvironmentBlendMode::OPAQUE);

    // Vulkan (vulkan_enable2)
    let vk_entry = unsafe { ash::Entry::load() }?;
    VK_ENTRY.set(vk_entry).ok();
    let vk_entry = VK_ENTRY.get().unwrap();
    let app_info = vk::ApplicationInfo::default().api_version(vk::make_api_version(0, 1, 1, 0));
    let vk_instance_raw = unsafe {
        xr_instance
            .create_vulkan_instance(
                system,
                get_instance_proc_addr,
                std::ptr::from_ref(&vk::InstanceCreateInfo::default().application_info(&app_info)).cast(),
            )?
            .map_err(vk::Result::from_raw)?
    };
    let vk_instance = unsafe { ash::Instance::load(vk_entry.static_fn(), vk::Instance::from_raw(vk_instance_raw as _)) };
    let phys_raw = unsafe { xr_instance.vulkan_graphics_device(system, vk_instance_raw as _)? };
    let physical_device = vk::PhysicalDevice::from_raw(phys_raw as _);
    let queue_family_index = unsafe {
        vk_instance
            .get_physical_device_queue_family_properties(physical_device)
            .iter()
            .enumerate()
            .find(|(_, q)| q.queue_flags.contains(vk::QueueFlags::GRAPHICS))
            .map(|(i, _)| i as u32)
            .ok_or_else(|| anyhow::anyhow!("no graphics queue family"))?
    };
    let priorities = [1.0f32];
    let queue_infos = [vk::DeviceQueueCreateInfo::default().queue_family_index(queue_family_index).queue_priorities(&priorities)];
    let device_create_info = vk::DeviceCreateInfo::default().queue_create_infos(&queue_infos);
    let vk_device_raw = unsafe {
        xr_instance
            .create_vulkan_device(system, get_instance_proc_addr, phys_raw as _, std::ptr::from_ref(&device_create_info).cast())?
            .map_err(vk::Result::from_raw)?
    };
    let device = unsafe { ash::Device::load(vk_instance.fp_v1_0(), vk::Device::from_raw(vk_device_raw as _)) };
    let queue = unsafe { device.get_device_queue(queue_family_index, 0) };
    let cmd_pool = unsafe {
        device.create_command_pool(
            &vk::CommandPoolCreateInfo::default().queue_family_index(queue_family_index).flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )?
    };
    let cmd = unsafe {
        device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default().command_pool(cmd_pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1),
        )?[0]
    };
    let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None)? };

    // Overlay session
    let (session, mut frame_waiter, mut frame_stream) = unsafe {
        let raw = create_overlay_session(
            &xr_instance,
            system,
            &xr::vulkan::SessionCreateInfo {
                instance: vk_instance_raw as _,
                physical_device: phys_raw as _,
                device: vk_device_raw as _,
                queue_family_index,
                queue_index: 0,
            },
        )
        .map_err(|e| anyhow::anyhow!("xrCreateSession (overlay) failed: {:?}", e))?;
        xr::Session::<xr::Vulkan>::from_raw(xr_instance.clone(), raw, Box::new(()))
    };
    let space = session.create_reference_space(xr::ReferenceSpaceType::LOCAL, xr::Posef::IDENTITY)?;
    let view_space = session.create_reference_space(xr::ReferenceSpaceType::VIEW, xr::Posef::IDENTITY)?;

    // Format + render pass
    let formats = session.enumerate_swapchain_formats()?;
    let preferred = [vk::Format::B8G8R8A8_SRGB, vk::Format::R8G8B8A8_SRGB, vk::Format::B8G8R8A8_UNORM, vk::Format::R8G8B8A8_UNORM];
    let format = preferred.into_iter().find(|w| formats.iter().any(|f| (*f as i64) == (w.as_raw() as i64))).unwrap_or(vk::Format::B8G8R8A8_SRGB);
    let srgb = matches!(format, vk::Format::B8G8R8A8_SRGB | vk::Format::R8G8B8A8_SRGB);

    let opacity: f32 = env::var("MONADO_FRAME_OPACITY").ok().and_then(|s| s.parse().ok()).unwrap_or(0.92);
    let alpha_mode = env::var("MONADO_FRAME_NO_ALPHA").is_err();
    let laser_on = env::var("MONADO_FRAME_NO_LASER").is_err();
    let panel_alpha = (opacity.clamp(0.0, 1.0) * 255.0) as u8;
    log::info!("format {:?} srgb={} alpha_mode={} opacity={} laser={}", format, srgb, alpha_mode, opacity, laser_on);

    let color_attachment = vk::AttachmentDescription::default()
        .format(format)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
        .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
    let color_ref = [vk::AttachmentReference::default().attachment(0).layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
    let subpass = [vk::SubpassDescription::default().pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS).color_attachments(&color_ref)];
    let attachments = [color_attachment];
    let render_pass = unsafe {
        device.create_render_pass(&vk::RenderPassCreateInfo::default().attachments(&attachments).subpasses(&subpass), None)?
    };
    RENDER_PASS.set(render_pass).ok();

    let allocator = Arc::new(Mutex::new(
        gpu_allocator::vulkan::Allocator::new(&gpu_allocator::vulkan::AllocatorCreateDesc {
            instance: vk_instance.clone(),
            device: device.clone(),
            physical_device,
            debug_settings: Default::default(),
            buffer_device_address: false,
            allocation_sizes: Default::default(),
        })
        .map_err(|e| anyhow::anyhow!("gpu-allocator init: {e}"))?,
    ));

    let mut settings_panel = make_panel(&session, &device, allocator.clone(), render_pass, format, srgb, (760, 620), (0.40, 0.40 * 620.0 / 760.0), posef([-0.38, 0.0, -1.0]))?;
    let mut gallery_panel = make_panel(&session, &device, allocator.clone(), render_pass, format, srgb, (1120, 900), (0.66, 0.66 * 900.0 / 1120.0), posef([0.0, 0.0, -1.0]))?;
    let mut wrist_panel = make_panel(&session, &device, allocator.clone(), render_pass, format, srgb, (400, 260), (0.11, 0.11 * 260.0 / 400.0), posef([0.0, 0.0, -1.0]))?;
    let mut photo_pool: Vec<PhotoSlot> = Vec::new();
    for _ in 0..3 {
        let gfx = make_panel(&session, &device, allocator.clone(), render_pass, format, srgb, (900, 820), (0.52, 0.52 * 820.0 / 900.0), posef([0.0, 0.0, -1.0]))?;
        photo_pool.push(PhotoSlot { gfx, open: false, photo: None, path: None, when: String::new() });
    }
    let mut laser = make_laser(&session, format)?;

    let mut settings = config::load();
    log::info!("loaded settings: enabled={} hold_ms={}", settings.enabled, settings.hold_ms);

    // Actions
    let action_set = xr_instance.create_action_set("monadoframe", "monado-frame controls", 0)?;
    let left_path = xr_instance.string_to_path("/user/hand/left")?;
    let right_path = xr_instance.string_to_path("/user/hand/right")?;
    let aim_action = action_set.create_action::<xr::Posef>("aim", "Aim pose", &[left_path, right_path])?;
    let grip_pose_action = action_set.create_action::<xr::Posef>("grippose", "Grip pose", &[left_path, right_path])?;
    let select_action = action_set.create_action::<f32>("select", "Select", &[left_path, right_path])?;
    let grab_action = action_set.create_action::<f32>("grab", "Grab", &[left_path, right_path])?;
    let system_action = action_set.create_action::<bool>("system", "System (show/hide)", &[left_path, right_path])?;
    let index_profile = xr_instance.string_to_path("/interaction_profiles/valve/index_controller")?;
    xr_instance.suggest_interaction_profile_bindings(
        index_profile,
        &[
            xr::Binding::new(&aim_action, xr_instance.string_to_path("/user/hand/left/input/aim/pose")?),
            xr::Binding::new(&aim_action, xr_instance.string_to_path("/user/hand/right/input/aim/pose")?),
            xr::Binding::new(&grip_pose_action, xr_instance.string_to_path("/user/hand/left/input/grip/pose")?),
            xr::Binding::new(&grip_pose_action, xr_instance.string_to_path("/user/hand/right/input/grip/pose")?),
            xr::Binding::new(&select_action, xr_instance.string_to_path("/user/hand/left/input/trigger/value")?),
            xr::Binding::new(&select_action, xr_instance.string_to_path("/user/hand/right/input/trigger/value")?),
            xr::Binding::new(&grab_action, xr_instance.string_to_path("/user/hand/left/input/squeeze/force")?),
            xr::Binding::new(&grab_action, xr_instance.string_to_path("/user/hand/right/input/squeeze/force")?),
            xr::Binding::new(&system_action, xr_instance.string_to_path("/user/hand/left/input/system/click")?),
            xr::Binding::new(&system_action, xr_instance.string_to_path("/user/hand/right/input/system/click")?),
        ],
    )?;
    session.attach_action_sets(&[&action_set])?;
    let aim_left = aim_action.create_space(&session, left_path, xr::Posef::IDENTITY)?;
    let aim_right = aim_action.create_space(&session, right_path, xr::Posef::IDENTITY)?;
    let grip_left = grip_pose_action.create_space(&session, left_path, xr::Posef::IDENTITY)?;

    // The wrist card is anchored to the left grip pose and is hand-locked: its
    // orientation is the head-facing one captured the moment you glance at it,
    // then frozen relative to the grip so it rotates WITH your wrist (it doesn't
    // chase the headset). It only shows while you look near it (a small FoV).
    // MONADO_FRAME_WRIST_POS="x,y,z" (metres, grip frame) places it.
    // MONADO_FRAME_WRIST_ROT="yaw,pitch,roll" (deg) forces a fixed orientation.
    // MONADO_FRAME_WRIST_FOV=<deg> sets the look-at half-angle (default 20).
    let wrist_offset_pos = env::var("MONADO_FRAME_WRIST_POS").ok().and_then(|s| parse3(&s)).unwrap_or([-0.05, 0.01, 0.05]);
    let wrist_rot: Option<[f32; 4]> =
        env::var("MONADO_FRAME_WRIST_ROT").ok().and_then(|s| parse3(&s)).map(|[y, p, r]| quat_from_euler_deg(y, p, r));
    let wrist_fov = env::var("MONADO_FRAME_WRIST_FOV").ok().and_then(|s| s.parse::<f32>().ok()).unwrap_or(20.0);
    let cos_show = wrist_fov.to_radians().cos();
    let cos_hide = (wrist_fov + 8.0).to_radians().cos();
    log::info!("wrist pos={wrist_offset_pos:?} fov={wrist_fov} fixed_rot={}", wrist_rot.is_some());

    let screenshots_dir = env::var("MONADO_SCREENSHOT_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/Pictures/Monado", env::var("HOME").unwrap_or_default()));
    log::info!("watching {} for new screenshots", screenshots_dir);
    let mut newest_seen = shots::scan_all(&screenshots_dir).first().map(|(_, m)| *m);
    let mut last_scan = Instant::now();
    const MAX_PENDING: usize = 6;
    let mut pending: Vec<Pending> = Vec::new(); // newest-first; wrist queue
    let mut pending_idx = 0usize;
    let mut grab: Option<(Target, usize, xr::Posef)> = None;
    let mut settings_visible = false; // summon with a SYSTEM double-press
    let mut gallery_visible = false;
    let mut gallery_paths: Vec<(PathBuf, String)> = Vec::new(); // all shots (path + date)
    let mut gallery_items: Vec<(egui::TextureHandle, String)> = Vec::new(); // current page
    let mut gallery_page = 0usize;
    let mut sys_prev = false;
    let mut last_sys_press: Option<Instant> = None;
    let mut wrist_lock: Option<[f32; 4]> = None; // frozen grip-relative orientation
    let mut wrist_shown = false; // gaze-gate hysteresis state

    log::info!("monado-frame ready. Point to interact, grip (force) to move a panel; Ctrl-C to quit.");

    let mut events = xr::EventDataBuffer::new();
    let mut running = false;
    let mut focused = false;

    loop {
        while let Some(event) = xr_instance.poll_event(&mut events)? {
            use xr::Event::*;
            match event {
                SessionStateChanged(e) => {
                    log::info!("session state -> {:?}", e.state());
                    focused = e.state() == xr::SessionState::FOCUSED;
                    match e.state() {
                        xr::SessionState::READY => {
                            session.begin(xr::ViewConfigurationType::PRIMARY_STEREO)?;
                            running = true;
                        }
                        xr::SessionState::STOPPING => {
                            session.end()?;
                            running = false;
                        }
                        xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING => return Ok(()),
                        _ => {}
                    }
                }
                InstanceLossPending(_) => return Ok(()),
                _ => {}
            }
        }
        if !running {
            std::thread::sleep(std::time::Duration::from_millis(100));
            continue;
        }

        let frame_state = frame_waiter.wait()?;
        frame_stream.begin()?;
        if !frame_state.should_render {
            frame_stream.end(frame_state.predicted_display_time, blend_mode, &[])?;
            continue;
        }
        let time = frame_state.predicted_display_time;

        // Watch for new screenshots (~1 Hz). Each new one becomes a wrist
        // notification (a thumbnail + date), queued newest-first.
        if last_scan.elapsed().as_secs_f32() > 1.0 {
            last_scan = Instant::now();
            let all = shots::scan_all(&screenshots_dir);
            let fresh: Vec<PathBuf> =
                all.iter().take_while(|(_, m)| newest_seen.is_none_or(|s| *m > s)).map(|(p, _)| p.clone()).collect();
            if let Some((_, m)) = all.first() {
                newest_seen = Some(*m);
            }
            for path in fresh.iter().rev() {
                let when = shots::shot_time(path);
                match shots::load_thumb(&wrist_panel.ctx, path, 256) {
                    Ok(thumb) => {
                        log::info!("new screenshot pending: {}", path.display());
                        pending.insert(0, Pending { path: path.clone(), when, thumb });
                    }
                    Err(e) => log::warn!("thumb {path:?}: {e}"),
                }
            }
            if !fresh.is_empty() {
                pending.truncate(MAX_PENDING);
                pending_idx = 0; // show the newest
            }
        }

        let hmd = locate_pose(&view_space, &space, time);

        // Pointer + grab + laser ray.
        type Ptr = Option<(f32, f32, bool)>;
        let mut ptr_settings: Ptr = None;
        let mut ptr_gallery: Ptr = None;
        let mut ptr_wrist: Ptr = None;
        let mut ptr_photo: [Ptr; 3] = [None, None, None];
        let mut wrist_ok = false; // wrist card has a valid follow pose this frame
        let mut laser_ray: Option<(xr::Posef, f32)> = None;
        // Only submit a panel's quad if its swapchain was actually acquired+released
        // this frame; otherwise xrEndFrame rejects an un-released swapchain.
        let mut settings_rendered = false;
        let mut gallery_rendered = false;
        let mut wrist_rendered = false;
        let mut photo_rendered = [false; 3];
        if focused {
            session.sync_actions(&[(&action_set).into()])?;

            // Double-press SYSTEM toggles panel visibility. A double tap of
            // right-system opens-then-closes WayVR (net no change) while
            // toggling ours once, so both can run without clashing.
            let sys_down = system_action.state(&session, left_path)?.current_state
                || system_action.state(&session, right_path)?.current_state;
            if sys_down && !sys_prev {
                let now = Instant::now();
                if last_sys_press.is_some_and(|t| now.duration_since(t).as_millis() < 400) {
                    settings_visible = !settings_visible;
                    last_sys_press = None;
                    grab = None;
                    if settings_visible {
                        if let Some(h) = hmd {
                            settings_panel.pose = front_pose(&h, 0.8, 0.0);
                        }
                    }
                    log::info!("settings panel {}", if settings_visible { "shown" } else { "hidden" });
                } else {
                    last_sys_press = Some(now);
                }
            }
            sys_prev = sys_down;

            let hands = [(left_path, &aim_left), (right_path, &aim_right)];

            // The wrist card rides the left hand (grip pose) while shots are
            // pending, but only shows when you look near it. Orientation is the
            // head-facing one frozen on the frame you start looking, so it then
            // turns with your wrist instead of chasing the headset.
            if !pending.is_empty() {
                if let (Some(gp), Some(h)) = (locate_pose(&grip_left, &space, time), hmd) {
                    let at = pose_compose(&gp, &xr::Posef { orientation: IDENTITY_QUAT, position: vec3f(wrist_offset_pos) });
                    let pos = [at.position.x, at.position.y, at.position.z];
                    let to_w = normalize([pos[0] - h.position.x, pos[1] - h.position.y, pos[2] - h.position.z]);
                    let cos_angle = dot(normalize(forward(&h)), to_w);
                    let looking = if wrist_shown { cos_angle > cos_hide } else { cos_angle > cos_show };
                    wrist_shown = looking;
                    if looking {
                        let orient = if let Some(q) = wrist_rot {
                            pose_compose(&gp, &xr::Posef { orientation: quatf(q), position: vec3f([0.0; 3]) }).orientation
                        } else {
                            let to_view = normalize([h.position.x - pos[0], h.position.y - pos[1], h.position.z - pos[2]]);
                            let x = normalize(cross([0.0, 1.0, 0.0], to_view));
                            let y = cross(to_view, x);
                            let bb = quat_from_axes(x, y, to_view);
                            let gq = qf(&gp.orientation);
                            let lock = *wrist_lock.get_or_insert_with(|| q_mul([-gq[0], -gq[1], -gq[2], gq[3]], bb));
                            quatf(q_mul(gq, lock))
                        };
                        wrist_panel.pose = xr::Posef { orientation: orient, position: at.position };
                        wrist_ok = true;
                    } else {
                        wrist_lock = None;
                    }
                } else {
                    wrist_shown = false;
                }
            } else {
                wrist_shown = false;
                wrist_lock = None;
            }

            // All interactable panels this frame (pose + size by target).
            let mut targets: Vec<(Target, xr::Posef, (f32, f32))> = Vec::new();
            if settings_visible {
                targets.push((Target::Settings, settings_panel.pose, settings_panel.size_m));
            }
            if gallery_visible {
                targets.push((Target::Gallery, gallery_panel.pose, gallery_panel.size_m));
            }
            for (i, s) in photo_pool.iter().enumerate() {
                if s.open {
                    targets.push((Target::Photo(i), s.gfx.pose, s.gfx.size_m));
                }
            }
            if wrist_ok {
                targets.push((Target::Wrist, wrist_panel.pose, wrist_panel.size_m));
            }

            if !targets.is_empty() {
                if let Some((target, h, rel)) = grab {
                    let (path, aim) = hands[h];
                    let held = grab_action.state(&session, path)?.current_state > GRAB_RELEASE;
                    match locate_pose(aim, &space, time) {
                        Some(pose) if held => {
                            let np = pose_compose(&pose, &rel);
                            match target {
                                Target::Settings => settings_panel.pose = np,
                                Target::Gallery => gallery_panel.pose = np,
                                Target::Photo(i) => photo_pool[i].gfx.pose = np,
                                Target::Wrist => {} // hand-locked, never grabbed
                            }
                        }
                        _ => grab = None,
                    }
                }

                if grab.is_none() {
                    for (i, (path, aim)) in hands.iter().enumerate() {
                        let Some(pose) = locate_pose(aim, &space, time) else { continue };
                        let mut best: Option<(Target, f32, (f32, f32))> = None;
                        for (tgt, ppose, psize) in &targets {
                            if let Some((u, v, t)) = raycast(&pose, ppose, *psize) {
                                if best.is_none_or(|(_, bt, _)| t < bt) {
                                    best = Some((*tgt, t, (u, v)));
                                }
                            }
                        }
                        if let Some((tgt, t, (u, v))) = best {
                            laser_ray = Some((pose, t));
                            let down = select_action.state(&session, *path)?.current_state > 0.5;
                            match tgt {
                                // Hand-locked: click to open, never grab.
                                Target::Wrist => ptr_wrist = Some((u, v, down)),
                                _ => {
                                    let grip = grab_action.state(&session, *path)?.current_state;
                                    if grab.is_none() && grip > GRAB_START {
                                        let pp = match tgt {
                                            Target::Settings => settings_panel.pose,
                                            Target::Gallery => gallery_panel.pose,
                                            Target::Photo(j) => photo_pool[j].gfx.pose,
                                            Target::Wrist => unreachable!(),
                                        };
                                        grab = Some((tgt, i, pose_compose(&pose_invert(&pose), &pp)));
                                        laser_ray = None;
                                    } else {
                                        match tgt {
                                            Target::Settings => ptr_settings = Some((u, v, down)),
                                            Target::Gallery => ptr_gallery = Some((u, v, down)),
                                            Target::Photo(j) => ptr_photo[j] = Some((u, v, down)),
                                            Target::Wrist => unreachable!(),
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let any_photo = photo_pool.iter().any(|s| s.open);
        if !settings_visible && !gallery_visible && !any_photo && !wrist_ok {
            frame_stream.end(time, blend_mode, &[])?;
            continue;
        }

        // Render settings + handle the gallery button.
        if settings_visible {
            let mut changed = false;
            let mut open_gallery = false;
            render_panel(&mut settings_panel, &device, cmd, cmd_pool, queue, fence, alpha_mode, ptr_settings, |ctx| {
                build_settings(ctx, &mut settings, &mut changed, &mut open_gallery, panel_alpha);
            })?;
            settings_rendered = true;
            let settings_down = ptr_settings.is_some_and(|(_, _, d)| d);
            if settings.dirty && !settings_down {
                config::save(&settings);
                settings.dirty = false;
            } else if changed {
                settings.dirty = true;
            }
            if open_gallery && !gallery_visible {
                gallery_visible = true;
                gallery_page = 0;
                gallery_paths = gallery_scan(&screenshots_dir);
                gallery_items = gallery_page_items(&gallery_panel.ctx, &gallery_paths, 0);
                if let Some(h) = hmd {
                    gallery_panel.pose = front_pose(&h, 0.85, 0.0);
                }
                log::info!("gallery opened ({} shots)", gallery_paths.len());
            }
        }

        // Render gallery + handle thumbnail clicks / paging.
        if gallery_visible {
            let mut gaction = GalleryAction::None;
            let page = gallery_page;
            let total = gallery_paths.len();
            render_panel(&mut gallery_panel, &device, cmd, cmd_pool, queue, fence, alpha_mode, ptr_gallery, |ctx| {
                build_gallery(ctx, &gallery_items, page, total, &mut gaction, panel_alpha);
            })?;
            gallery_rendered = true;
            match gaction {
                GalleryAction::Close => gallery_visible = false,
                GalleryAction::PrevPage => {
                    gallery_page = gallery_page.saturating_sub(1);
                    gallery_items = gallery_page_items(&gallery_panel.ctx, &gallery_paths, gallery_page);
                }
                GalleryAction::NextPage => {
                    gallery_page += 1;
                    gallery_items = gallery_page_items(&gallery_panel.ctx, &gallery_paths, gallery_page);
                }
                GalleryAction::Open(k) => {
                    let global = gallery_page * GALLERY_PER + k;
                    if let Some((path, when)) = gallery_paths.get(global).map(|(p, w)| (p.clone(), w.clone())) {
                        open_photo(&mut photo_pool, &path, &when, hmd);
                    }
                }
                GalleryAction::None => {}
            }
        }

        // Render each open floating photo panel + handle its actions.
        for i in 0..photo_pool.len() {
            if !photo_pool[i].open {
                continue;
            }
            let mut paction = PhotoAction::None;
            let tex = photo_pool[i].photo.as_ref().map(|p| p.handle.clone());
            let when = photo_pool[i].when.clone();
            let ptr = ptr_photo[i];
            render_panel(&mut photo_pool[i].gfx, &device, cmd, cmd_pool, queue, fence, alpha_mode, ptr, |ctx| {
                build_photo(ctx, tex.as_ref(), &when, &mut paction, panel_alpha);
            })?;
            photo_rendered[i] = true;
            match paction {
                PhotoAction::Copy => {
                    if let Some(p) = &photo_pool[i].path {
                        shots::copy_to_clipboard(&p.to_string_lossy());
                    }
                }
                PhotoAction::Delete => {
                    if let Some(p) = photo_pool[i].path.clone() {
                        let _ = fs::remove_file(&p);
                        log::info!("deleted {}", p.display());
                        if gallery_visible {
                            gallery_paths = gallery_scan(&screenshots_dir);
                            gallery_page = gallery_page.min(gallery_paths.len().saturating_sub(1) / GALLERY_PER);
                            gallery_items = gallery_page_items(&gallery_panel.ctx, &gallery_paths, gallery_page);
                        }
                    }
                    close_slot(&mut photo_pool[i]);
                }
                PhotoAction::Dismiss => close_slot(&mut photo_pool[i]),
                PhotoAction::None => {}
            }
        }

        // Render the wrist card; scroll/open the pending queue.
        if wrist_ok {
            let mut waction = WristAction::None;
            let thumb = pending.get(pending_idx).map(|p| p.thumb.clone());
            let when = pending.get(pending_idx).map(|p| p.when.clone()).unwrap_or_default();
            let idx = pending_idx;
            let total = pending.len();
            render_panel(&mut wrist_panel, &device, cmd, cmd_pool, queue, fence, alpha_mode, ptr_wrist, |ctx| {
                waction = build_wrist(ctx, thumb.as_ref(), &when, idx, total, panel_alpha);
            })?;
            wrist_rendered = true;
            match waction {
                WristAction::Older => {
                    if pending_idx + 1 < pending.len() {
                        pending_idx += 1;
                    }
                }
                WristAction::Newer => pending_idx = pending_idx.saturating_sub(1),
                WristAction::Open => {
                    if let Some((path, when)) = pending.get(pending_idx).map(|p| (p.path.clone(), p.when.clone())) {
                        open_photo(&mut photo_pool, &path, &when, hmd);
                        pending.remove(pending_idx);
                        if pending_idx >= pending.len() {
                            pending_idx = pending.len().saturating_sub(1);
                        }
                        log::info!("wrist -> opened photo ({} still pending)", pending.len());
                    }
                }
                WristAction::None => {}
            }
        }

        // Laser texture (only when we have a ray to draw).
        let laser_ready = if laser_on && laser_ray.is_some() {
            fill_laser(&mut laser, &device, cmd, queue, fence).is_ok()
        } else {
            false
        };

        // Submit only the quads whose swapchains were released this frame.
        let mut quads: Vec<xr::CompositionLayerQuad<xr::Vulkan>> = Vec::new();
        if settings_rendered {
            quads.push(quad_layer(&settings_panel, &space, alpha_mode));
        }
        if gallery_rendered {
            quads.push(quad_layer(&gallery_panel, &space, alpha_mode));
        }
        for i in 0..photo_pool.len() {
            if photo_pool[i].open && photo_rendered[i] {
                quads.push(quad_layer(&photo_pool[i].gfx, &space, alpha_mode));
            }
        }
        if wrist_rendered && !pending.is_empty() {
            quads.push(quad_layer(&wrist_panel, &space, alpha_mode));
        }
        let q_laser = match (laser_ready, laser_ray, hmd) {
            (true, Some((aim, t)), Some(h)) => Some(laser_quad(&laser, &space, &aim, t, &h)),
            _ => None,
        };
        let mut layers: Vec<&xr::CompositionLayerBase<xr::Vulkan>> = Vec::new();
        for q in &quads {
            layers.push(q);
        }
        if let Some(q) = &q_laser {
            layers.push(q);
        }
        frame_stream.end(time, blend_mode, &layers)?;
    }
}

fn posef(p: [f32; 3]) -> xr::Posef {
    xr::Posef { orientation: xr::Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 }, position: xr::Vector3f { x: p[0], y: p[1], z: p[2] } }
}

unsafe fn create_overlay_session(
    instance: &xr::Instance,
    system: xr::SystemId,
    info: &xr::vulkan::SessionCreateInfo,
) -> std::result::Result<xr::sys::Session, xr::sys::Result> {
    use xr::sys::Handle;
    let overlay = xr::sys::SessionCreateInfoOverlayEXTX {
        ty: xr::sys::SessionCreateInfoOverlayEXTX::TYPE,
        next: std::ptr::null(),
        create_flags: xr::OverlaySessionCreateFlagsEXTX::EMPTY,
        session_layers_placement: 5,
    };
    let binding = xr::sys::GraphicsBindingVulkanKHR {
        ty: xr::sys::GraphicsBindingVulkanKHR::TYPE,
        next: (&raw const overlay).cast(),
        instance: info.instance,
        physical_device: info.physical_device,
        device: info.device,
        queue_family_index: info.queue_family_index,
        queue_index: info.queue_index,
    };
    let create_info = xr::sys::SessionCreateInfo {
        ty: xr::sys::SessionCreateInfo::TYPE,
        next: (&raw const binding).cast(),
        create_flags: xr::SessionCreateFlags::default(),
        system_id: system,
    };
    let mut out = xr::sys::Session::NULL;
    let r = (instance.fp().create_session)(instance.as_raw(), &raw const create_info, &raw mut out);
    if r.into_raw() >= 0 {
        Ok(out)
    } else {
        Err(r)
    }
}
