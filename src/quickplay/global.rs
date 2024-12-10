use std::{
    collections::HashMap,
    net::SocketAddr,
    str::FromStr,
    sync::{Arc, RwLock, RwLockReadGuard},
    time::Duration,
};

use bitflags::bitflags;
use serde::Deserialize;
use tracing::{debug, error};

use crate::configuration::Configuration;

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

#[derive(Deserialize, Debug)]
struct SchemaOuterJson {
    schema: SchemaInnerJson,
}

#[derive(Deserialize, Debug)]
struct SchemaInnerJson {
    map_gamemodes: HashMap<String, String>,
}

struct Schema {
    map_gamemodes: HashMap<String, Gamemodes>,
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

    #[derive(Debug, PartialEq, Clone, Copy)]
    pub struct Gamemodes: u8 {
        const PAYLOAD        = 1 << 0;
        const KOTH           = 1 << 1;
        const ATTACK_DEFENSE = 1 << 2;
        const CTF            = 1 << 3;
        const CAPTURE_POINT  = 1 << 4;
        const PAYLOAD_RACE   = 1 << 5;
        const ALTERNATIVE    = 1 << 6;
        const ARENA          = 1 << 7;
    }
}

impl Gamemodes {
    /// Returns a single gamemode corresponding to the name provided in
    /// `gamemode`, or None if there is no matching gamemode.
    pub fn from_str(gamemode: &str) -> Option<Gamemodes> {
        match gamemode {
            "attack_defense" => Some(Self::ATTACK_DEFENSE),
            "ctf" => Some(Self::CTF),
            "capture_point" => Some(Self::CAPTURE_POINT),
            "koth" => Some(Self::KOTH),
            "payload" => Some(Self::PAYLOAD),
            "payload_race" => Some(Self::PAYLOAD_RACE),
            "arena" => Some(Self::ARENA),
            "alternative" => Some(Self::ALTERNATIVE),

            // "special_events" => Self::...,
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct ServerInfo {
    pub addr: SocketAddr,

    pub players: u8,
    pub max_players: u8,

    pub map: String, // TODO: intern this?
    pub gamemode: Gamemodes,

    pub tags: ServerTags,

    pub score: f32,
    pub _ping: f32,
}

pub struct QuickplayGlobal {
    servers: Arc<RwLock<Vec<ServerInfo>>>,
}

impl QuickplayGlobal {
    pub fn new(configuration: &'static Configuration) -> Self {
        let servers = Arc::new(RwLock::new(vec![]));
        let self_ = Self {
            servers: servers.clone(),
        };
        std::thread::spawn(move || Self::run_background(servers, configuration));

        self_
    }

    pub fn server_list(&self) -> RwLockReadGuard<Vec<ServerInfo>> {
        self.servers.read().unwrap()
    }

    fn run_background(
        servers: Arc<RwLock<Vec<ServerInfo>>>,
        configuration: &'static Configuration,
    ) {
        loop {
            match Self::update_schema(configuration) {
                Ok(schema) => match Self::update_server_info(configuration, &schema) {
                    Ok(server_list) => {
                        *servers.write().unwrap() = server_list;
                    }
                    Err(e) => error!("error while updating server list: {e}"),
                },
                Err(e) => error!("error while updating schema: {e}"),
            }

            std::thread::sleep(Duration::from_secs(10));
        }
    }

    fn update_server_info(
        configuration: &Configuration,
        schema: &Schema,
    ) -> anyhow::Result<Vec<ServerInfo>> {
        let server_list_data = std::fs::read(&configuration.quickplay.server_list_path)?;
        let server_list = parse_server_infos_from_json(&server_list_data, schema)?;

        debug!(
            "loaded server list from file: {} servers total",
            server_list.len()
        );

        Ok(server_list)
    }

    fn update_schema(configuration: &Configuration) -> anyhow::Result<Schema> {
        let schema_data = std::fs::read(&configuration.quickplay.schema_path)?;

        debug!("loaded schema from file");

        parse_schema_from_json(&schema_data)
    }
}

fn parse_schema_from_json(schema_data: &[u8]) -> anyhow::Result<Schema> {
    let raw_schema: SchemaOuterJson = serde_json::from_slice(schema_data)?;

    Ok(Schema {
        map_gamemodes: raw_schema
            .schema
            .map_gamemodes
            .into_iter()
            .filter_map(|(k, v)| Some((k, Gamemodes::from_str(&v)?)))
            .collect(),
    })
}

fn parse_server_infos_from_json(data: &[u8], schema: &Schema) -> anyhow::Result<Vec<ServerInfo>> {
    let raw_servers: Vec<ServerInfoJson> = serde_json::from_slice(data)?;
    let parsed_servers = raw_servers
        .into_iter()
        .filter_map(|raw_server| {
            let gamemode = *schema.map_gamemodes.get(&raw_server.map)?;
            Some(ServerInfo {
                addr: SocketAddr::from_str(&raw_server.addr).ok()?,
                players: raw_server.players.try_into().ok()?,
                max_players: raw_server.max_players.try_into().ok()?,
                map: raw_server.map,
                gamemode,
                tags: string_tags_to_flags(&raw_server.gametype),
                score: raw_server.score,
                _ping: raw_server.ping,
            })
        })
        .collect::<Vec<ServerInfo>>();

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
    assert_eq!(
        string_tags_to_flags(&["nocrits".to_string()]),
        ServerTags::NO_CRITS
    );
    assert_eq!(
        string_tags_to_flags(&["norespawntime".to_string(), "dummy_tag".to_string()]),
        ServerTags::NO_RESPAWN_TIMES
    );

    assert_eq!(string_tags_to_flags(&[]), ServerTags::empty());
}
