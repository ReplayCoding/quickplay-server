use std::{net::SocketAddr, str::FromStr};

use bitflags::bitflags;
use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct ServerListJson {
    servers: Vec<ServerInfoJson>,
}

#[derive(Deserialize, Debug)]
struct ServerInfoJson {
    addr: String,

    players: u32,
    max_players: u32,

    map: String,
    gametype: Vec<String>, // tags

    score: f32,
    ping: f32,
}

bitflags! {
    #[derive(Debug, PartialEq, Clone, Copy)]
    pub struct ServerTags : u8 {
            const NO_CRITS         = 1 << 0;
            const NO_RESPAWN_TIMES = 1 << 1;
            const RESPAWN_TIMES    = 1 << 2;
            const RTD              = 1 << 3;
            const CLASS_LIMITS     = 1 << 4;
            const CLASS_BANS       = 1 << 5;
            const NO_OBJECTIVES    = 1 << 6;
    }
}

#[derive(Debug)]
pub struct ServerInfo {
    pub addr: SocketAddr,

    pub players: u8,
    pub max_players: u8,

    pub map: String, // TODO: intern this?

    pub tags: ServerTags,

    pub score: f32,
    pub ping: f32,
}

pub fn load_server_infos_from_json(data: &[u8]) -> anyhow::Result<Vec<ServerInfo>> {
    let raw_servers: ServerListJson = serde_json::from_slice(data)?;
    let parsed_servers = raw_servers
        .servers
        .into_iter()
        .map(|raw_server| {
            Ok(ServerInfo {
                addr: SocketAddr::from_str(&raw_server.addr)?,
                players: raw_server.players.try_into()?,
                max_players: raw_server.max_players.try_into()?,
                map: raw_server.map,
                tags: string_tags_to_flags(&raw_server.gametype),
                score: raw_server.score,
                ping: raw_server.ping,
            })
        })
        .collect::<anyhow::Result<Vec<ServerInfo>>>()?;

    Ok(parsed_servers)
}

fn string_tags_to_flags(str_tags: &[String]) -> ServerTags {
    let mut tags = ServerTags::empty();

    for str_tag in str_tags {
        tags |= match str_tag.as_str() {
            "nocrits" => ServerTags::NO_CRITS,
            "norespawntime" => ServerTags::NO_RESPAWN_TIMES,
            "respawntimes" => ServerTags::RESPAWN_TIMES,
            "rtd" => ServerTags::RTD,
            "classlimits" => ServerTags::CLASS_LIMITS,
            "classbans" => ServerTags::CLASS_BANS,
            "nocap" => ServerTags::NO_OBJECTIVES,
            _ => continue,
        }
    }

    tags
}

#[test]
fn test_string_tags_to_flags() {
    // TODO
}
