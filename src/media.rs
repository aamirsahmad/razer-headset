//! Media transport control.
//!
//! The headset's Play/Pause button is an HID *input*: it tells the host what
//! to do, and the host's media player is what actually plays or pauses. There
//! is no report that makes the headset itself play or pause. So we do what the
//! button ultimately does: drive the active media player over MPRIS (D-Bus).

use std::error::Error;
use std::fmt;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedValue;

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";

#[derive(Debug, Clone, Copy)]
pub enum Action {
    PlayPause,
    Play,
    Pause,
    Next,
    Previous,
}

impl Action {
    fn method(self) -> &'static str {
        match self {
            Self::PlayPause => "PlayPause",
            Self::Play => "Play",
            Self::Pause => "Pause",
            Self::Next => "Next",
            Self::Previous => "Previous",
        }
    }
}

#[derive(Debug)]
pub enum MediaError {
    NoPlayers,
    Bus(zbus::Error),
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPlayers => write!(f, "no MPRIS media player is running"),
            Self::Bus(e) => write!(f, "D-Bus error: {e}"),
        }
    }
}

impl Error for MediaError {}

impl From<zbus::Error> for MediaError {
    fn from(e: zbus::Error) -> Self {
        Self::Bus(e)
    }
}

pub struct Player {
    name: String,
    status: String,
}

impl Player {
    pub fn name(&self) -> &str {
        self.name.strip_prefix(MPRIS_PREFIX).unwrap_or(&self.name)
    }
    pub fn status(&self) -> &str {
        &self.status
    }
}

pub struct Media {
    conn: Connection,
}

impl Media {
    pub fn connect() -> Result<Self, MediaError> {
        Ok(Self {
            conn: Connection::session()?,
        })
    }

    fn player_proxy(&self, name: &str) -> Result<Proxy<'_>, MediaError> {
        Ok(Proxy::new(
            &self.conn,
            name.to_owned(),
            MPRIS_PATH,
            PLAYER_IFACE,
        )?)
    }

    /// Every MPRIS player on the session bus, with its PlaybackStatus.
    pub fn players(&self) -> Result<Vec<Player>, MediaError> {
        let dbus = zbus::blocking::fdo::DBusProxy::new(&self.conn)?;
        let mut players = Vec::new();
        for name in dbus.list_names().map_err(zbus::Error::from)? {
            let name = name.to_string();
            if !name.starts_with(MPRIS_PREFIX) || name == "org.mpris.MediaPlayer2.playerctld" {
                continue;
            }
            let status = self
                .player_proxy(&name)?
                .get_property::<OwnedValue>("PlaybackStatus")
                .ok()
                .and_then(|v| String::try_from(v).ok())
                .unwrap_or_else(|| "Unknown".into());
            players.push(Player { name, status });
        }
        Ok(players)
    }

    /// The player a media key should go to: a playing one if any, else the first.
    fn target(&self) -> Result<Player, MediaError> {
        let mut players = self.players()?;
        if players.is_empty() {
            return Err(MediaError::NoPlayers);
        }
        let idx = players
            .iter()
            .position(|p| p.status == "Playing")
            .unwrap_or(0);
        Ok(players.swap_remove(idx))
    }

    /// Perform `action` on the target player and return which player got it.
    pub fn send(&self, action: Action) -> Result<Player, MediaError> {
        let player = self.target()?;
        self.player_proxy(&player.name)?
            .call_method(action.method(), &())?;
        Ok(player)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_name_strips_mpris_prefix() {
        let p = Player {
            name: "org.mpris.MediaPlayer2.firefox.instance_1".into(),
            status: "Playing".into(),
        };
        assert_eq!(p.name(), "firefox.instance_1");
        assert_eq!(p.status(), "Playing");

        let raw = Player {
            name: "custom.player".into(),
            status: "Paused".into(),
        };
        assert_eq!(raw.name(), "custom.player");
    }

    #[test]
    fn action_methods_match_mpris_spec() {
        assert_eq!(Action::PlayPause.method(), "PlayPause");
        assert_eq!(Action::Play.method(), "Play");
        assert_eq!(Action::Pause.method(), "Pause");
        assert_eq!(Action::Next.method(), "Next");
        assert_eq!(Action::Previous.method(), "Previous");
    }
}
