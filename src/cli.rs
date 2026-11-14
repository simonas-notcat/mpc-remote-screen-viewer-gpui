use crate::{preferences::ViewerSettings, protocol::DEFAULT_PORT};
use zeroize::Zeroizing;

pub struct Launch {
    pub host: String,
    pub port: u16,
    pub password: Zeroizing<String>,
    pub audio: bool,
    pub auto_connect: bool,
    pub help: bool,
    host_supplied: bool,
    port_supplied: bool,
}
impl Launch {
    /// Explicit options override saved defaults. Environment only supplies a password.
    /// Error messages intentionally do not echo argument values or credentials.
    pub fn parse(
        args: impl IntoIterator<Item = String>,
        environment_password: Option<String>,
    ) -> Result<Self, String> {
        let mut launch = Self {
            host: String::new(),
            port: DEFAULT_PORT,
            password: Zeroizing::new(environment_password.unwrap_or_default()),
            audio: false,
            auto_connect: false,
            help: false,
            host_supplied: false,
            port_supplied: false,
        };
        let mut args = args.into_iter();
        while let Some(option) = args.next() {
            match option.as_str() {
                "--host" => {
                    launch.host = args.next().ok_or("--host requires a value")?;
                    launch.host_supplied = true;
                }
                "--port" => {
                    launch.port = args
                        .next()
                        .ok_or("--port requires a value")?
                        .parse::<u16>()
                        .ok()
                        .filter(|&p| p > 0)
                        .ok_or("Port must be in 1..=65535")?;
                    launch.port_supplied = true;
                }
                "--password" => {
                    launch.password =
                        Zeroizing::new(args.next().ok_or("--password requires a value")?)
                }
                "--audio" => launch.audio = true,
                "--help" | "-h" => launch.help = true,
                _ => return Err("Unknown option; use --help".into()),
            }
        }
        launch.auto_connect =
            launch.host_supplied && !launch.host.trim().is_empty() && !launch.password.is_empty();
        Ok(launch)
    }
    /// Overlay only explicit address options; restoring a host never auto-connects.
    /// Keep parsing/help independent of filesystem access and saved settings.
    pub fn apply_preferences(&mut self, settings: &mut ViewerSettings) {
        if self.host_supplied {
            let _ = settings.remember_address(&self.host, settings.port);
        } else {
            self.host.clone_from(&settings.host);
        }
        if self.port_supplied {
            settings.port = self.port;
            settings.show_port |= self.port != DEFAULT_PORT;
        } else {
            self.port = settings.port;
        }
    }
}
pub const HELP: &str = "MPC Remote Screen Viewer GPUI (development build)\n\nUsage: mpc-remote-screen-viewer-gpui [--host HOST] [--port PORT] [--password PASSWORD] [--audio]\n\nPrefer the masked UI or REMOTE_SCREEN_PASSWORD to a command-line password.\n--host auto-connects only with a nonempty password; manual Connect permits empty passwords.\n--audio enables device audio playback when the service advertises audio support.\nUse only on a trusted LAN or through an external VPN; the device protocol has no encryption.";

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(args: &[&str], env: Option<&str>) -> Launch {
        Launch::parse(args.iter().map(|s| s.to_string()), env.map(str::to_owned)).unwrap()
    }
    #[test]
    fn preferences_are_defaults_not_overrides_or_autoconnect_requests() {
        let saved = ViewerSettings {
            host: "saved-device".into(),
            port: 17447,
            actual_size: true,
            control_on: true,
            ..ViewerSettings::default()
        };
        let mut settings = saved.clone();
        let mut launch = parse(&[], Some("env"));
        launch.apply_preferences(&mut settings);
        assert_eq!(launch.host, "saved-device");
        assert_eq!(launch.port, 17447);
        assert_eq!(&*launch.password, "env");
        assert_eq!(settings, saved);
        assert!(!launch.auto_connect);

        let mut launch = parse(&["--host", "new-device"], Some("env"));
        launch.apply_preferences(&mut settings);
        assert_eq!(launch.host, "new-device");
        assert_eq!(launch.port, 17447);
        assert!(launch.auto_connect);
        assert!(settings.actual_size && settings.control_on);
        assert_eq!(settings.host, "new-device");

        let mut launch = parse(&["--port", "7447", "--password", ""], Some("env"));
        launch.apply_preferences(&mut settings);
        assert_eq!(launch.host, "new-device");
        assert_eq!(launch.port, DEFAULT_PORT);
        assert_eq!(settings.port, DEFAULT_PORT);
        assert!(launch.password.is_empty());
        assert!(!launch.auto_connect);

        let mut launch = parse(&["--host", "", "--port", "17448", "--audio"], None);
        launch.apply_preferences(&mut settings);
        assert!(settings.host.is_empty());
        assert_eq!(settings.port, 17448);
        assert!(settings.show_port && launch.audio);
        assert!(!launch.auto_connect);
    }
    #[test]
    fn exact_password_precedence_and_autoconnect() {
        let l = parse(&["--host", "mpc", "--password", "  p  "], Some("env"));
        assert_eq!(&*l.password, "  p  ");
        assert!(l.auto_connect);
        assert!(!parse(&["--host", "mpc"], None).auto_connect);
        assert!(parse(&["--host", "mpc"], Some("env")).auto_connect);
        assert!(!parse(&[], Some("env")).auto_connect);
        for p in ["0", "65536", "-1", "abc"] {
            assert!(Launch::parse(["--port".into(), p.into()], None).is_err());
        }
        assert_eq!(
            Launch::parse(["sensitive-value".into()], None)
                .err()
                .unwrap(),
            "Unknown option; use --help"
        );
    }
}
