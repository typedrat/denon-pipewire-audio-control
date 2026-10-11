//! The Denon AVR telnet control protocol: `\r`-terminated ASCII lines on TCP port 23.

use std::fmt;

use crate::scale::Mv;

pub const PORT: u16 = 23;

/// Ends every line in both directions.
pub const TERMINATOR: char = '\r';

/// Sent after connecting so the state is known without waiting for something to change.
pub const QUERIES: [&str; 4] = ["ZM?", "SI?", "MV?", "MU?"];

/// Sent periodically: a TCP write is the only way to notice a receiver that dropped off the
/// network without closing the connection.
pub const HEARTBEAT: &str = "ZM?";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Power(bool),
    Input(String),
    Volume(Mv),
    Mute(bool),
}

/// Parses one line from the receiver; lines this daemon doesn't track yield `None`.
pub fn parse_line(line: &str) -> Option<Event> {
    match line {
        "ZMON" => return Some(Event::Power(true)),
        "ZMOFF" | "PWSTANDBY" => return Some(Event::Power(false)),
        "MUON" => return Some(Event::Mute(true)),
        "MUOFF" => return Some(Event::Mute(false)),
        _ => {}
    }
    if let Some(digits) = line.strip_prefix("MV") {
        return Mv::parse(digits).map(Event::Volume);
    }
    if let Some(input) = line.strip_prefix("SI")
        && !input.is_empty()
    {
        return Some(Event::Input(input.to_owned()));
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Volume(Mv),
    Mute(bool),
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Command::Volume(mv) => write!(f, "MV{mv}"),
            Command::Mute(true) => f.write_str("MUON"),
            Command::Mute(false) => f.write_str("MUOFF"),
        }
    }
}

/// Reassembles lines from TCP reads, which can split or merge them arbitrarily.
#[derive(Debug, Default)]
pub struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    pub fn push(&mut self, data: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(data);
        let mut lines = Vec::new();
        while let Some(end) = self.pending.iter().position(|&b| b == TERMINATOR as u8) {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let text = String::from_utf8_lossy(&line[..end]).trim().to_owned();
            if !text.is_empty() {
                lines.push(text);
            }
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tracked_events() {
        assert_eq!(parse_line("ZMON"), Some(Event::Power(true)));
        assert_eq!(parse_line("ZMOFF"), Some(Event::Power(false)));
        assert_eq!(parse_line("PWSTANDBY"), Some(Event::Power(false)));
        assert_eq!(parse_line("SIBD"), Some(Event::Input("BD".into())));
        assert_eq!(parse_line("SISAT/CBL"), Some(Event::Input("SAT/CBL".into())));
        assert_eq!(parse_line("MV62"), Some(Event::Volume(Mv::from_whole(62))));
        assert_eq!(parse_line("MV605"), Some(Event::Volume(Mv::from_half_steps(121))));
        assert_eq!(parse_line("MUON"), Some(Event::Mute(true)));
        assert_eq!(parse_line("MUOFF"), Some(Event::Mute(false)));
    }

    #[test]
    fn ignores_everything_else() {
        assert_eq!(parse_line("MVMAX 98"), None);
        assert_eq!(parse_line("PWON"), None);
        assert_eq!(parse_line("SVOFF"), None);
        assert_eq!(parse_line("SI"), None);
        assert_eq!(parse_line("Z2MU OFF"), None);
    }

    #[test]
    fn formats_commands() {
        assert_eq!(Command::Volume(Mv::from_whole(62)).to_string(), "MV62");
        assert_eq!(Command::Volume(Mv::from_half_steps(121)).to_string(), "MV605");
        assert_eq!(Command::Mute(true).to_string(), "MUON");
        assert_eq!(Command::Mute(false).to_string(), "MUOFF");
    }

    #[test]
    fn reassembles_split_lines() {
        let mut lines = LineBuffer::default();
        assert_eq!(lines.push(b"MV6"), Vec::<String>::new());
        assert_eq!(lines.push(b"2\rMU"), vec!["MV62"]);
        assert_eq!(lines.push(b"OFF\r\nSIBD\r"), vec!["MUOFF", "SIBD"]);
    }
}
