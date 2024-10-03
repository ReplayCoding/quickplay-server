use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct QuickplayConfiguration {
    /// the prefix that all user preferences will start with
    pub preference_convar_prefix: String,
    /// the path to the server list
    pub server_list_path: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ServerConfiguration {
    /// the address and port that the server will bind to
    pub bind_address: String,

    /// the delay between connection background updates, in milliseconds
    pub connection_update_delay: u64,
    /// the maximum time that a connection can live before being forcibly
    /// killed, in milliseconds
    pub connection_timeout: u64,
    /// the number of tasks for recieving packets
    pub num_packet_tasks: usize,

    /// number of bytes that can be sent in a single packet of reliable data
    pub max_reliable_packet_size: u32,
    /// maximum number of packets that can be sent before the netchannel will
    /// start dropping every packet
    pub max_packets_dropped: u32,

    /// the appid to advertise through A2S
    pub app_id: u16,
    /// the app description to advertise through A2S
    pub app_desc: String,
    /// the app version to advertise through A2S
    pub app_version: String,
    /// the server name to advertise through A2S
    pub server_name: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Configuration {
    pub server: ServerConfiguration,
    pub quickplay: QuickplayConfiguration,
}

impl Configuration {
    pub fn load_default() -> Self {
        Self {
            server: ServerConfiguration {
                bind_address: "127.0.0.2:4444".to_string(),

                connection_update_delay: 150,
                connection_timeout: 20_000,
                num_packet_tasks: 8,

                max_reliable_packet_size: 1024,
                max_packets_dropped: 5000,

                app_id: 440,
                app_desc: "Team Fortress 2".to_string(),
                app_version: "".to_string(),
                server_name: "Quickplay".to_string(),
            },
            quickplay: QuickplayConfiguration {
                preference_convar_prefix: "rqp_".to_string(),
                server_list_path: "/invalid/path.json".to_string(),
            },
        }
    }

    pub fn load_from_file(path: &Path) -> anyhow::Result<Self> {
        let data = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&data)?)
    }
}
