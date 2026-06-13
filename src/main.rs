// monado-frame — in-headset overlay for Monado.
//
// Two independently grabbable panels rendered with egui over an OpenXR overlay
// session: a settings panel (toggles the finger-frame gesture, writes
// ~/.config/monado/gestures.json) and a screenshot-review panel (watches
// ~/Pictures/Monado, shows the latest shot with copy/delete/dismiss + history).

use std::env;
use std::fs;
use std::os::raw::c_char;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use anyhow::{bail, Result};
use ash::vk;
use ash::vk::Handle as _;
use openxr as xr;

static VK_ENTRY: OnceLock<ash::Entry> = OnceLock::new();

const PPP: f32 = 1.5; // egui points -> pixels (crisper text on a VR panel)

// Grab uses the grip FORCE sensor (not the soft capacitive grip, which saturates
// on Index/Knuckles with a light curl). Hysteresis: needs a firm squeeze to start,
// a looser hold to keep.
const GRAB_START: f32 = 0.40;
const GRAB_RELEASE: f32 = 0.15;

// Material-You-ish dark tonal palette.
mod theme {
    use egui::Color32;
    pub const PRIMARY: Color32 = Color32::from_rgb(160, 200, 255);
    pub const SURFACE: Color32 = Color32::from_rgb(19, 19, 24);
    pub const SURFACE_CONTAINER: Color32 = Color32::from_rgb(32, 31, 39);
    pub const SURFACE_CONTAINER_HIGH: Color32 = Color32::from_rgb(43, 42, 51);
    pub const ON_SURFACE: Color32 = Color32::from_rgb(230, 225, 233);
    pub const ON_SURFACE_VAR: Color32 = Color32::from_rgb(196, 199, 209);
}

unsafe extern "system" fn get_instance_proc_addr(
    instance: xr::sys::platform::VkInstance,
    name: *const c_char,
) -> Option<unsafe extern "system" fn()> {
    let entry = VK_ENTRY.get().expect("vk entry not initialised");
    let vk_instance = vk::Instance::from_raw(instance as _);
    (entry.static_fn().get_instance_proc_addr)(vk_instance, name)
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

struct Settings {
    enabled: bool,
    hold_ms: i32,
    debug: bool,
    path: String,
    dirty: bool,
}

fn config_path() -> String {
    let base = env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/.config", env::var("HOME").unwrap_or_default()));
    format!("{base}/monado/gestures.json")
}

fn load_settings() -> Settings {
    let path = config_path();
    let mut s = Settings { enabled: true, hold_ms: 2000, debug: false, path: path.clone(), dirty: false };
    if let Ok(txt) = fs::read_to_string(&path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            if let Some(b) = v.get("enabled").and_then(|x| x.as_bool()) {
                s.enabled = b;
            }
            if let Some(n) = v.get("hold_ms").and_then(|x| x.as_i64()) {
                s.hold_ms = n as i32;
            }
            if let Some(b) = v.get("debug").and_then(|x| x.as_bool()) {
                s.debug = b;
            }
        }
    }
    s
}

fn save_settings(s: &Settings) {
    if let Some(dir) = std::path::Path::new(&s.path).parent() {
        let _ = fs::create_dir_all(dir);
    }
    let v = serde_json::json!({ "enabled": s.enabled, "hold_ms": s.hold_ms, "debug": s.debug });
    match serde_json::to_string_pretty(&v) {
        Ok(txt) => match fs::write(&s.path, txt) {
            Ok(()) => log::info!("wrote {} (enabled={} hold_ms={})", s.path, s.enabled, s.hold_ms),
            Err(e) => log::warn!("failed to write {}: {e}", s.path),
        },
        Err(e) => log::warn!("serialise config: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Screenshots
// ---------------------------------------------------------------------------

struct Photo {
    name: String,
    handle: egui::TextureHandle,
}

enum PhotoAction {
    None,
    Copy,
    Delete,
    Dismiss,
}

// All *.png in a dir, newest first, with mtimes.
fn scan_all(dir: &str) -> Vec<(PathBuf, SystemTime)> {
    let mut v = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            let is_png = p.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("png"));
            if !is_png {
                continue;
            }
            if let Ok(m) = e.metadata().and_then(|md| md.modified()) {
                v.push((p, m));
            }
        }
    }
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

fn load_photo(ctx: &egui::Context, path: &std::path::Path) -> Result<Photo> {
    let img = image::open(path)?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    let handle = ctx.load_texture("screenshot", color, egui::TextureOptions::LINEAR);
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("screenshot").to_string();
    Ok(Photo { name, handle })
}

fn copy_to_clipboard(path: &str) {
    match fs::File::open(path) {
        Ok(file) => match std::process::Command::new("wl-copy")
            .arg("--type")
            .arg("image/png")
            .stdin(std::process::Stdio::from(file))
            .spawn()
        {
            Ok(_) => log::info!("copied {path} to clipboard"),
            Err(e) => log::warn!("wl-copy failed ({e}); is wl-clipboard installed?"),
        },
        Err(e) => log::warn!("copy: cannot open {path}: {e}"),
    }
}

// ---------------------------------------------------------------------------
// Styling (Material-ish dark)
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

    let pill = CornerRadius::same(16);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = pill;
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

fn bar_frame() -> egui::Frame {
    egui::Frame::default().fill(theme::SURFACE_CONTAINER_HIGH).inner_margin(egui::Margin::symmetric(16, 12))
}

fn panel_frame() -> egui::Frame {
    egui::Frame::default().fill(theme::SURFACE).inner_margin(egui::Margin::same(16))
}

fn app_bar(ctx: &egui::Context, left: String, right: Option<String>) {
    egui::TopBottomPanel::top("appbar").frame(bar_frame()).show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(left).size(20.0).strong().color(egui::Color32::WHITE));
            if let Some(r) = right {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(r).color(theme::ON_SURFACE_VAR));
                });
            }
        });
    });
}

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::default()
        .fill(theme::SURFACE_CONTAINER)
        .corner_radius(16)
        .inner_margin(16)
        .shadow(egui::Shadow { offset: [0, 3], blur: 14, spread: 0, color: egui::Color32::from_black_alpha(110) })
        .show(ui, add)
        .inner
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

// ---------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum PanelKind {
    Settings,
    Photo,
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

    let range = vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    };
    let mut framebuffers = Vec::with_capacity(images.len());
    for &img in &images {
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
        framebuffers.push(fb);
    }

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

    Ok(PanelGfx {
        swapchain,
        framebuffers,
        ctx,
        renderer,
        px,
        size_m,
        pose,
        prev_pos: None,
        prev_down: false,
    })
}

#[allow(clippy::too_many_arguments)]
fn render_panel(
    p: &mut PanelGfx,
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    cmd_pool: vk::CommandPool,
    queue: vk::Queue,
    fence: vk::Fence,
    glass: bool,
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
            painter.circle_filled(ps, 5.0, egui::Color32::from_rgb(122, 178, 255));
            painter.circle_stroke(ps, 5.0, egui::Stroke::new(1.5, egui::Color32::from_black_alpha(140)));
        }
    });

    let prims = p.ctx.tessellate(out.shapes, out.pixels_per_point);
    p.renderer
        .set_textures(queue, cmd_pool, &out.textures_delta.set)
        .map_err(|e| anyhow::anyhow!("set_textures: {e}"))?;

    let index = p.swapchain.acquire_image()?;
    p.swapchain.wait_image(xr::Duration::INFINITE)?;
    unsafe {
        device.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())?;
        device.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;
        let clear_color = if glass { [0.0, 0.0, 0.0, 0.0] } else { [0.05, 0.06, 0.08, 1.0] };
        let clear = [vk::ClearValue { color: vk::ClearColorValue { float32: clear_color } }];
        let rp = vk::RenderPassBeginInfo::default()
            .render_pass(RENDER_PASS.get().copied().unwrap())
            .framebuffer(p.framebuffers[index as usize])
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: vk::Extent2D { width: p.px.0, height: p.px.1 },
            })
            .clear_values(&clear);
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
    p.renderer
        .free_textures(&out.textures_delta.free)
        .map_err(|e| anyhow::anyhow!("free_textures: {e}"))?;
    p.swapchain.release_image()?;
    Ok(())
}

// Shared render pass handle (set once; read in render_panel).
static RENDER_PASS: OnceLock<vk::RenderPass> = OnceLock::new();

fn quad_layer<'a>(p: &'a PanelGfx, space: &'a xr::Space, glass: bool) -> xr::CompositionLayerQuad<'a, xr::Vulkan> {
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
    if glass {
        q = q.layer_flags(xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA);
    }
    q
}

// ---------------------------------------------------------------------------
// UI builders
// ---------------------------------------------------------------------------

fn build_settings(ctx: &egui::Context, s: &mut Settings, changed: &mut bool) {
    use egui_phosphor::regular as icons;
    app_bar(ctx, format!("{}  monado-frame", icons::GEAR_SIX), None);
    egui::CentralPanel::default().frame(panel_frame()).show(ctx, |ui| {
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("Finger-frame gesture").color(theme::ON_SURFACE_VAR));
        });
        ui.add_space(12.0);
        card(ui, |ui| {
            *changed |= ui.checkbox(&mut s.enabled, "Gesture enabled").changed();
            ui.add_space(14.0);
            ui.label(egui::RichText::new("Hold delay").color(theme::ON_SURFACE_VAR));
            ui.add_space(4.0);
            *changed |= ui.add(egui::Slider::new(&mut s.hold_ms, 500..=4000).suffix(" ms")).changed();
        });
        ui.add_space(14.0);
        ui.vertical_centered(|ui| {
            ui.small(egui::RichText::new(&s.path).color(theme::ON_SURFACE_VAR));
        });
    });
}

#[allow(clippy::too_many_arguments)]
fn build_photo(
    ctx: &egui::Context,
    photo: Option<&Photo>,
    count: usize,
    idx: usize,
    action: &mut PhotoAction,
    nav: &mut i32,
) {
    use egui_phosphor::regular as icons;
    app_bar(ctx, format!("{}  Screenshot", icons::CAMERA), Some(format!("{}/{}", idx + 1, count.max(1))));
    egui::TopBottomPanel::bottom("photo_bottom").frame(bar_frame()).show(ctx, |ui| {
        ui.horizontal(|ui| {
            let prev = egui::Button::new(egui::RichText::new(icons::CARET_LEFT).size(20.0));
            if ui.add_enabled(idx + 1 < count, prev).on_hover_text("Older").clicked() {
                *nav = 1;
            }
            let next = egui::Button::new(egui::RichText::new(icons::CARET_RIGHT).size(20.0));
            if ui.add_enabled(idx > 0, next).on_hover_text("Newer").clicked() {
                *nav = -1;
            }
            ui.separator();
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
    egui::CentralPanel::default().frame(panel_frame()).show(ctx, |ui| {
        ui.centered_and_justified(|ui| {
            if let Some(p) = photo {
                let resp = ui.add(egui::Image::new(&p.handle).max_size(ui.available_size() * 0.92).corner_radius(8));
                paint_corner_brackets(ui.painter(), resp.rect.expand(8.0), 24.0, egui::Stroke::new(2.5, theme::PRIMARY));
            } else {
                ui.label(egui::RichText::new("No screenshot").color(theme::ON_SURFACE_VAR));
            }
        });
    });
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
    // -------- OpenXR instance + system --------
    let entry = xr::Entry::linked();
    let available = entry.enumerate_extensions()?;
    if !available.khr_vulkan_enable2 {
        bail!("runtime is missing XR_KHR_vulkan_enable2");
    }
    if !available.extx_overlay {
        bail!("runtime is missing XR_EXTX_overlay (needed for an overlay app)");
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

    // -------- Vulkan (vulkan_enable2) --------
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
    let vk_instance =
        unsafe { ash::Instance::load(vk_entry.static_fn(), vk::Instance::from_raw(vk_instance_raw as _)) };

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
    let queue_infos = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(queue_family_index)
        .queue_priorities(&priorities)];
    let device_create_info = vk::DeviceCreateInfo::default().queue_create_infos(&queue_infos);
    let vk_device_raw = unsafe {
        xr_instance
            .create_vulkan_device(
                system,
                get_instance_proc_addr,
                phys_raw as _,
                std::ptr::from_ref(&device_create_info).cast(),
            )?
            .map_err(vk::Result::from_raw)?
    };
    let device = unsafe { ash::Device::load(vk_instance.fp_v1_0(), vk::Device::from_raw(vk_device_raw as _)) };
    let queue = unsafe { device.get_device_queue(queue_family_index, 0) };

    let cmd_pool = unsafe {
        device.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(queue_family_index)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )?
    };
    let cmd = unsafe {
        device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(cmd_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )?[0]
    };
    let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None)? };

    // -------- Overlay session --------
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

    // -------- Format + render pass --------
    let formats = session.enumerate_swapchain_formats()?;
    let preferred = [
        vk::Format::B8G8R8A8_SRGB,
        vk::Format::R8G8B8A8_SRGB,
        vk::Format::B8G8R8A8_UNORM,
        vk::Format::R8G8B8A8_UNORM,
    ];
    let format = preferred
        .into_iter()
        .find(|w| formats.iter().any(|f| (*f as i64) == (w.as_raw() as i64)))
        .unwrap_or(vk::Format::B8G8R8A8_SRGB);
    let srgb = matches!(format, vk::Format::B8G8R8A8_SRGB | vk::Format::R8G8B8A8_SRGB);

    let opacity: f32 = env::var("MONADO_FRAME_OPACITY").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0);
    let glass = opacity < 0.999;
    log::info!("format {:?} (srgb={}), glass={}", format, srgb, glass);

    let color_attachment = vk::AttachmentDescription::default()
        .format(format)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
        .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
    let color_ref = [vk::AttachmentReference::default()
        .attachment(0)
        .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)];
    let subpass = [vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&color_ref)];
    let attachments = [color_attachment];
    let render_pass = unsafe {
        device.create_render_pass(
            &vk::RenderPassCreateInfo::default().attachments(&attachments).subpasses(&subpass),
            None,
        )?
    };
    RENDER_PASS.set(render_pass).ok();

    // -------- Shared egui allocator + the two panels --------
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

    let mut settings_panel = make_panel(
        &session,
        &device,
        allocator.clone(),
        render_pass,
        format,
        srgb,
        (760, 560),
        (0.40, 0.40 * 560.0 / 760.0),
        posef([-0.38, 0.0, -1.0]),
    )?;
    let mut photo_panel = make_panel(
        &session,
        &device,
        allocator.clone(),
        render_pass,
        format,
        srgb,
        (900, 820),
        (0.52, 0.52 * 820.0 / 900.0),
        posef([0.30, 0.0, -0.95]),
    )?;

    let mut settings = load_settings();
    log::info!("loaded settings: enabled={} hold_ms={}", settings.enabled, settings.hold_ms);

    // -------- Input actions --------
    let action_set = xr_instance.create_action_set("monadoframe", "monado-frame controls", 0)?;
    let left_path = xr_instance.string_to_path("/user/hand/left")?;
    let right_path = xr_instance.string_to_path("/user/hand/right")?;
    let aim_action = action_set.create_action::<xr::Posef>("aim", "Aim pose", &[left_path, right_path])?;
    let select_action = action_set.create_action::<f32>("select", "Select", &[left_path, right_path])?;
    let grab_action = action_set.create_action::<f32>("grab", "Grab", &[left_path, right_path])?;
    let index_profile = xr_instance.string_to_path("/interaction_profiles/valve/index_controller")?;
    xr_instance.suggest_interaction_profile_bindings(
        index_profile,
        &[
            xr::Binding::new(&aim_action, xr_instance.string_to_path("/user/hand/left/input/aim/pose")?),
            xr::Binding::new(&aim_action, xr_instance.string_to_path("/user/hand/right/input/aim/pose")?),
            xr::Binding::new(&select_action, xr_instance.string_to_path("/user/hand/left/input/trigger/value")?),
            xr::Binding::new(&select_action, xr_instance.string_to_path("/user/hand/right/input/trigger/value")?),
            xr::Binding::new(&grab_action, xr_instance.string_to_path("/user/hand/left/input/squeeze/force")?),
            xr::Binding::new(&grab_action, xr_instance.string_to_path("/user/hand/right/input/squeeze/force")?),
        ],
    )?;
    session.attach_action_sets(&[&action_set])?;
    let aim_left = aim_action.create_space(&session, left_path, xr::Posef::IDENTITY)?;
    let aim_right = aim_action.create_space(&session, right_path, xr::Posef::IDENTITY)?;

    // -------- State --------
    let screenshots_dir = env::var("MONADO_SCREENSHOT_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/Pictures/Monado", env::var("HOME").unwrap_or_default()));
    log::info!("watching {} for new screenshots", screenshots_dir);
    let mut newest_seen = scan_all(&screenshots_dir).first().map(|(_, m)| *m);
    let mut last_scan = Instant::now();
    let mut history: Vec<PathBuf> = Vec::new();
    let mut hist_idx = 0usize;
    let mut current_photo: Option<Photo> = None;
    let mut photo_visible = false;

    // Active grab: (which panel, hand index, panel pose relative to that hand's aim).
    let mut grab: Option<(PanelKind, usize, xr::Posef)> = None;

    log::info!("monado-frame ready. Point to interact, grip to move a panel; Ctrl-C to quit.");

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

        // ---- Watch for new screenshots (~1 Hz) ----
        if last_scan.elapsed().as_secs_f32() > 1.0 {
            last_scan = Instant::now();
            let all = scan_all(&screenshots_dir);
            if let Some((path, mtime)) = all.first() {
                if newest_seen.is_none_or(|s| *mtime > s) {
                    newest_seen = Some(*mtime);
                    history = all.iter().map(|(p, _)| p.clone()).collect();
                    hist_idx = 0;
                    match load_photo(&photo_panel.ctx, path) {
                        Ok(p) => {
                            log::info!("new screenshot: {}", p.name);
                            current_photo = Some(p);
                            photo_visible = true;
                        }
                        Err(e) => log::warn!("load screenshot {path:?}: {e}"),
                    }
                }
            }
        }

        // ---- Pointer + grab ----
        let mut ptr_settings: Option<(f32, f32, bool)> = None;
        let mut ptr_photo: Option<(f32, f32, bool)> = None;
        if focused {
            session.sync_actions(&[(&action_set).into()])?;
            let hands = [(left_path, &aim_left), (right_path, &aim_right)];

            // Continue/release an active grab.
            if let Some((kind, h, rel)) = grab {
                let (path, aim) = hands[h];
                let held = grab_action.state(&session, path)?.current_state > GRAB_RELEASE;
                match locate_pose(aim, &space, time) {
                    Some(pose) if held => {
                        let np = pose_compose(&pose, &rel);
                        match kind {
                            PanelKind::Settings => settings_panel.pose = np,
                            PanelKind::Photo => photo_panel.pose = np,
                        }
                    }
                    _ => grab = None,
                }
            }

            // Each non-grabbing hand points at its nearest visible panel.
            for (i, (path, aim)) in hands.iter().enumerate() {
                if matches!(grab, Some((_, gh, _)) if gh == i) {
                    continue;
                }
                let Some(pose) = locate_pose(aim, &space, time) else { continue };

                let mut best: Option<(PanelKind, f32, (f32, f32))> = None;
                if let Some((u, v, t)) = raycast(&pose, &settings_panel.pose, settings_panel.size_m) {
                    best = Some((PanelKind::Settings, t, (u, v)));
                }
                if photo_visible {
                    if let Some((u, v, t)) = raycast(&pose, &photo_panel.pose, photo_panel.size_m) {
                        if best.is_none_or(|(_, bt, _)| t < bt) {
                            best = Some((PanelKind::Photo, t, (u, v)));
                        }
                    }
                }

                if let Some((kind, _, (u, v))) = best {
                    let grip = grab_action.state(&session, *path)?.current_state;
                    if grab.is_none() && grip > GRAB_START {
                        let panel_pose = match kind {
                            PanelKind::Settings => settings_panel.pose,
                            PanelKind::Photo => photo_panel.pose,
                        };
                        grab = Some((kind, i, pose_compose(&pose_invert(&pose), &panel_pose)));
                    } else {
                        let down = select_action.state(&session, *path)?.current_state > 0.5;
                        match kind {
                            PanelKind::Settings => ptr_settings = Some((u, v, down)),
                            PanelKind::Photo => ptr_photo = Some((u, v, down)),
                        }
                    }
                }
            }
        }

        // ---- Render settings panel ----
        let mut changed = false;
        render_panel(&mut settings_panel, &device, cmd, cmd_pool, queue, fence, glass, ptr_settings, |ctx| {
            build_settings(ctx, &mut settings, &mut changed);
        })?;
        let settings_down = ptr_settings.is_some_and(|(_, _, d)| d);
        if settings.dirty && !settings_down {
            save_settings(&settings);
            settings.dirty = false;
        } else if changed {
            settings.dirty = true;
        }

        // ---- Render photo panel ----
        let mut photo_action = PhotoAction::None;
        let mut nav = 0i32;
        if photo_visible {
            let count = history.len();
            let idx = hist_idx;
            let photo_ref = current_photo.as_ref();
            render_panel(&mut photo_panel, &device, cmd, cmd_pool, queue, fence, glass, ptr_photo, |ctx| {
                build_photo(ctx, photo_ref, count, idx, &mut photo_action, &mut nav);
            })?;

            if nav != 0 && !history.is_empty() {
                hist_idx = (hist_idx as i32 + nav).clamp(0, history.len() as i32 - 1) as usize;
                if let Ok(p) = load_photo(&photo_panel.ctx, &history[hist_idx]) {
                    current_photo = Some(p);
                }
            }
            match photo_action {
                PhotoAction::Copy => {
                    if let Some(path) = history.get(hist_idx) {
                        copy_to_clipboard(&path.to_string_lossy());
                    }
                }
                PhotoAction::Delete => {
                    if let Some(path) = history.get(hist_idx).cloned() {
                        let _ = fs::remove_file(&path);
                        log::info!("deleted {}", path.display());
                        history = scan_all(&screenshots_dir).into_iter().map(|(p, _)| p).collect();
                        if history.is_empty() {
                            photo_visible = false;
                            current_photo = None;
                        } else {
                            hist_idx = hist_idx.min(history.len() - 1);
                            if let Ok(p) = load_photo(&photo_panel.ctx, &history[hist_idx]) {
                                current_photo = Some(p);
                            }
                        }
                    }
                }
                PhotoAction::Dismiss => photo_visible = false,
                PhotoAction::None => {}
            }
        }

        // ---- Submit layers ----
        let q_settings = quad_layer(&settings_panel, &space, glass);
        if photo_visible {
            let q_photo = quad_layer(&photo_panel, &space, glass);
            frame_stream.end(time, blend_mode, &[&q_settings, &q_photo])?;
        } else {
            frame_stream.end(time, blend_mode, &[&q_settings])?;
        }
    }
}

fn posef(p: [f32; 3]) -> xr::Posef {
    xr::Posef {
        orientation: xr::Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
        position: xr::Vector3f { x: p[0], y: p[1], z: p[2] },
    }
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

fn quat_rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
    let tx = 2.0 * (y * v[2] - z * v[1]);
    let ty = 2.0 * (z * v[0] - x * v[2]);
    let tz = 2.0 * (x * v[1] - y * v[0]);
    [
        v[0] + w * tx + (y * tz - z * ty),
        v[1] + w * ty + (z * tx - x * tz),
        v[2] + w * tz + (x * ty - y * tx),
    ]
}

fn q_mul(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    let (ax, ay, az, aw) = (a[0], a[1], a[2], a[3]);
    let (bx, by, bz, bw) = (b[0], b[1], b[2], b[3]);
    [
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ]
}

fn qf(q: &xr::Quaternionf) -> [f32; 4] {
    [q.x, q.y, q.z, q.w]
}

fn pose_compose(a: &xr::Posef, b: &xr::Posef) -> xr::Posef {
    let q = q_mul(qf(&a.orientation), qf(&b.orientation));
    let rp = quat_rotate(qf(&a.orientation), [b.position.x, b.position.y, b.position.z]);
    xr::Posef {
        orientation: xr::Quaternionf { x: q[0], y: q[1], z: q[2], w: q[3] },
        position: xr::Vector3f { x: a.position.x + rp[0], y: a.position.y + rp[1], z: a.position.z + rp[2] },
    }
}

fn pose_invert(a: &xr::Posef) -> xr::Posef {
    let iq = [-a.orientation.x, -a.orientation.y, -a.orientation.z, a.orientation.w];
    let ip = quat_rotate(iq, [a.position.x, a.position.y, a.position.z]);
    xr::Posef {
        orientation: xr::Quaternionf { x: iq[0], y: iq[1], z: iq[2], w: iq[3] },
        position: xr::Vector3f { x: -ip[0], y: -ip[1], z: -ip[2] },
    }
}

fn locate_pose(aim: &xr::Space, base: &xr::Space, time: xr::Time) -> Option<xr::Posef> {
    let loc = aim.locate(base, time).ok()?;
    let need = xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID;
    if loc.location_flags.contains(need) {
        Some(loc.pose)
    } else {
        None
    }
}

// Raycast a controller aim pose onto a quad; returns (u, v, distance) if it hits.
fn raycast(pose: &xr::Posef, quad: &xr::Posef, size_m: (f32, f32)) -> Option<(f32, f32, f32)> {
    let o = [pose.position.x, pose.position.y, pose.position.z];
    let q = qf(&pose.orientation);
    let qq = qf(&quad.orientation);
    let dir = quat_rotate(q, [0.0, 0.0, -1.0]);
    let normal = quat_rotate(qq, [0.0, 0.0, 1.0]);
    let axis_x = quat_rotate(qq, [1.0, 0.0, 0.0]);
    let axis_y = quat_rotate(qq, [0.0, 1.0, 0.0]);
    let c = [quad.position.x, quad.position.y, quad.position.z];

    let denom = dir[0] * normal[0] + dir[1] * normal[1] + dir[2] * normal[2];
    if denom.abs() < 1e-6 {
        return None;
    }
    let co = [c[0] - o[0], c[1] - o[1], c[2] - o[2]];
    let t = (co[0] * normal[0] + co[1] * normal[1] + co[2] * normal[2]) / denom;
    if t <= 0.0 {
        return None;
    }
    let p = [o[0] + dir[0] * t, o[1] + dir[1] * t, o[2] + dir[2] * t];
    let off = [p[0] - c[0], p[1] - c[1], p[2] - c[2]];
    let lx = off[0] * axis_x[0] + off[1] * axis_x[1] + off[2] * axis_x[2];
    let ly = off[0] * axis_y[0] + off[1] * axis_y[1] + off[2] * axis_y[2];
    if lx.abs() > size_m.0 * 0.5 || ly.abs() > size_m.1 * 0.5 {
        return None;
    }
    Some((lx / size_m.0 + 0.5, 0.5 - ly / size_m.1, t))
}
