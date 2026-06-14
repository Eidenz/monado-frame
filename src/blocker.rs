// Input arbitration: while you're pointing at one of our panels, stop the game
// from also receiving controller input. Uses libmonado (dlopen'd at runtime;
// gracefully off if unavailable) to set IO blocks on the game client.
//
// The game is the active + visible session that is NOT an overlay — so this
// naturally skips us and other overlays (e.g. WayVR) without name-matching.
use libmonado::{BlockFlags, ClientLogic, ClientState, Monado, Version};

pub struct Blocker {
    monado: Option<Monado>,
    use_io_blocks: bool,
    blocked: bool,
}

impl Blocker {
    pub fn new() -> Self {
        match Monado::auto_connect() {
            Ok(m) => {
                let use_io_blocks = m.get_api_version() >= Version::new(1, 6, 0);
                let b = Self { monado: Some(m), use_io_blocks, blocked: false };
                b.apply(false); // clear any stale blocks left by a previous crash
                log::info!("input arbitration on (Monado connected, io_blocks={use_io_blocks})");
                b
            }
            Err(e) => {
                log::warn!("input arbitration off: can't reach Monado via libmonado ({e})");
                Self { monado: None, use_io_blocks: false, blocked: false }
            }
        }
    }

    /// Block or release game input. Edge-triggered: only talks to Monado on change.
    pub fn set(&mut self, block: bool) {
        if block == self.blocked {
            return;
        }
        self.blocked = block;
        self.apply(block);
    }

    fn apply(&self, block: bool) {
        let Some(monado) = &self.monado else { return };
        let Ok(clients) = monado.clients() else { return };
        for mut client in clients {
            let Ok(state) = client.state() else { continue };
            let is_game = state.contains(ClientState::ClientSessionActive)
                && state.contains(ClientState::ClientSessionVisible)
                && !state.contains(ClientState::ClientSessionOverlay);
            if !is_game {
                continue;
            }
            let _ = if self.use_io_blocks {
                let flags = if block { BlockFlags::BlockInputs } else { BlockFlags::None };
                client.set_io_blocks(flags.into())
            } else {
                client.set_io_active(!block)
            };
        }
    }
}

impl Drop for Blocker {
    fn drop(&mut self) {
        if self.blocked {
            self.apply(false);
        }
    }
}
