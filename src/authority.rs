use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "PascalCase")]
pub enum Authority {
    Client,
    Server,
}

impl fmt::Display for Authority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Authority::Client => "Client",
            Authority::Server => "Server",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_flag_value_is_lowercase_and_the_wire_value_is_capitalized() {
        let parsed = Authority::from_str("server", true).expect("server should parse");
        assert_eq!(parsed, Authority::Server);
        assert_eq!(serde_json::to_value(parsed).unwrap(), json!("Server"));
        assert_eq!(
            serde_json::from_value::<Authority>(json!("Client")).unwrap(),
            Authority::Client
        );
        assert!(serde_json::from_value::<Authority>(json!("client")).is_err());
    }
}
