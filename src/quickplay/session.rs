// official comfig quickplay implementation
//     server list downloader -> cloudflare worker -> clientside filtering?
//     serverlist downloader
//         grabs server list from steam api and ranks + filters
//             no SDR
//             steamid
//             max, max players
//             version check
//             casual map rotation
//             server tags
//             password
//         random shuffle?
//         calculates server pings from a2s?
//             final ping is "real ping" - "ideal ping" (based on distance)
//         generates list of servers and sends to worker
//
//     cloudflare worker
//         receives serverlist updates, stores that in DB
//         handles client requests for list
//             recalculates ping based on real client ping + "ideal" client ping (is it the same formula as serverlist?)
//             always returns entire list?
//     clientside
//         ...
// my quickplay implementation
//     can't run anything on client, do everything server-side
//     use official server list downloader -> merged worker + client
//     how to calculate latency?
//         measuring netchannel ack time?
//     reimplement all client filtering, provided through setinfo
//         max 255 convars can be sent in initial message, minus existing.
//         CLIENT WILL CRASH IF IT TRIES TO SEND MORE, so need to be careful
//         most prefs can be represented by a single var
//         map bans & gamemode need one var per slot
//             official client has 6 map ban slots
//             8 gamemodes
//         9 (?) single var + 6 map bans + 8 gamemodes = 23 total, way under

use std::{net::SocketAddr, sync::Arc};

use bitflags::bitflags;
use num_enum::TryFromPrimitive;
use tracing::trace;

use crate::{
    configuration::Configuration,
    quickplay::global::{QuickplayGlobal, ServerInfo, ServerTags},
};

// Maybe move this to config?
const MAX_MAP_BANS: usize = 6;

const SERVER_HEADROOM: u16 = 1;
const FULL_PLAYERS: u16 = 24;

#[derive(Debug, TryFromPrimitive)]
#[repr(u8)]
enum RandomCritsPreference {
    Disabled = 0,
    Enabled,
    DontCare,
}

#[derive(Debug, TryFromPrimitive)]
#[repr(u8)]
enum RespawnTimesPreference {
    Default = 0,
    Instant,
    DontCare,
}

#[derive(Debug, TryFromPrimitive)]
#[repr(u8)]
enum RtdPreference {
    Disabled = 0,
    Enabled,
    DontCare,
}

#[derive(Debug, TryFromPrimitive)]
#[repr(u8)]
enum ClassRestrictionsPreference {
    None = 0,
    Limits,
    LimitsAndBans,
}

#[derive(Debug, TryFromPrimitive)]
#[repr(u8)]
enum ObjectivesPreference {
    Disabled = 0,
    Enabled,
    DontCare,
}

enum PreferenceDecodeError {
    UnparseableValue = 0,
    InvalidValue,
    UnknownPreference,
}

bitflags! {
    #[derive(Debug)]
    struct Gamemodes: u8 {
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

#[derive(Debug)]
struct QuickplayPreferences {
    random_crits: RandomCritsPreference,
    respawn_times: RespawnTimesPreference,
    rtd: RtdPreference,

    class_restrictions: ClassRestrictionsPreference,
    objectives: ObjectivesPreference,

    party_size: u8,

    map_bans: [Option<String>; MAX_MAP_BANS],
    _gamemodes: Gamemodes,
}

impl QuickplayPreferences {
    fn update_preference(&mut self, name: &str, value: &str) -> Result<(), PreferenceDecodeError> {
        const MAP_BAN_PREF_NAME: &str = "map_ban_";
        if let Some(slot_index_str) = name.strip_prefix(MAP_BAN_PREF_NAME) {
            let slot_index = slot_index_str
                .parse::<usize>()
                .map_err(|_| PreferenceDecodeError::UnknownPreference)?;

            if slot_index >= self.map_bans.len() {
                return Err(PreferenceDecodeError::UnknownPreference);
            }

            self.map_bans[slot_index] = Some(value.to_owned());
        } else {
            match name {
                "random_crits" => {
                    self.random_crits = decode_enum_preference::<RandomCritsPreference>(value)?
                }
                "respawn_times" => {
                    self.respawn_times = decode_enum_preference::<RespawnTimesPreference>(value)?
                }
                "rtd" => self.rtd = decode_enum_preference::<RtdPreference>(value)?,
                "class_restrictions" => {
                    self.class_restrictions =
                        decode_enum_preference::<ClassRestrictionsPreference>(value)?
                }
                "objectives" => {
                    self.objectives = decode_enum_preference::<ObjectivesPreference>(value)?
                }
                "party_size" => {
                    self.party_size = value
                        .parse::<u8>()
                        .map_err(|_| PreferenceDecodeError::UnparseableValue)?
                }
                _ => return Err(PreferenceDecodeError::UnknownPreference),
            }
        };

        Ok(())
    }

    fn build_filter_tags(&self) -> (ServerTags, ServerTags) {
        let mut must_match_tags = ServerTags::empty();
        let mut must_not_match_tags = ServerTags::empty();

        match self.random_crits {
            RandomCritsPreference::Enabled => must_not_match_tags |= ServerTags::NO_CRITS,
            RandomCritsPreference::Disabled => must_match_tags |= ServerTags::NO_CRITS,
            RandomCritsPreference::DontCare => {}
        }

        // FIXME: is this actually correct?
        match self.respawn_times {
            RespawnTimesPreference::Default => {
                must_not_match_tags |= ServerTags::RESPAWN_TIMES | ServerTags::NO_RESPAWN_TIMES
            }
            RespawnTimesPreference::Instant => must_match_tags |= ServerTags::NO_RESPAWN_TIMES,
            RespawnTimesPreference::DontCare => {}
        }

        match self.rtd {
            RtdPreference::Enabled => must_match_tags |= ServerTags::RTD,
            RtdPreference::Disabled => must_not_match_tags |= ServerTags::RTD,
            RtdPreference::DontCare => {}
        }

        match self.class_restrictions {
            ClassRestrictionsPreference::None => {
                must_not_match_tags |= ServerTags::CLASS_LIMITS | ServerTags::CLASS_BANS
            }
            ClassRestrictionsPreference::Limits => must_match_tags |= ServerTags::CLASS_BANS,
            ClassRestrictionsPreference::LimitsAndBans => {}
        }

        match self.objectives {
            ObjectivesPreference::Enabled => must_not_match_tags |= ServerTags::NO_OBJECTIVES,
            ObjectivesPreference::Disabled => must_match_tags |= ServerTags::NO_OBJECTIVES,
            ObjectivesPreference::DontCare => {}
        }

        (must_match_tags, must_not_match_tags)
    }
}

impl Default for QuickplayPreferences {
    fn default() -> Self {
        Self {
            random_crits: RandomCritsPreference::DontCare,
            respawn_times: RespawnTimesPreference::Default,
            rtd: RtdPreference::Disabled,
            class_restrictions: ClassRestrictionsPreference::None,
            objectives: ObjectivesPreference::Enabled,
            party_size: 1,
            map_bans: std::array::from_fn(|_| None),
            // Everything except for alternative and arena
            _gamemodes: Gamemodes::all() ^ (Gamemodes::ALTERNATIVE | Gamemodes::ARENA),
        }
    }
}

pub struct QuickplaySession {
    preferences: QuickplayPreferences,

    global: Arc<QuickplayGlobal>,
    configuration: &'static Configuration,
}

impl QuickplaySession {
    pub fn new(global: Arc<QuickplayGlobal>, configuration: &'static Configuration) -> Self {
        Self {
            preferences: QuickplayPreferences::default(),

            global,
            configuration,
        }
    }

    pub fn update_preferences_from_convars(
        &mut self,
        convars: &[(String, String)],
    ) -> Result<(), String> {
        for (name, value) in convars {
            // source doesn't let you undefine convars made with setinfo. as a
            // workaround, if the user makes the value an empty string it will
            // be treated as if it doesn't exist.
            if value.is_empty() {
                continue;
            }

            let prefix = &self.configuration.quickplay.preference_convar_prefix;

            if name.starts_with(prefix) {
                let name = &name[prefix.len()..];
                if let Err(err_type) = self.preferences.update_preference(name, value) {
                    return Err(match err_type {
                        PreferenceDecodeError::UnparseableValue => {
                            format!("Could not decode value for preference \"{name}\": \"{value}\"")
                        }
                        PreferenceDecodeError::InvalidValue => {
                            format!("Invalid value for preference \"{name}\": \"{value}\"")
                        }
                        PreferenceDecodeError::UnknownPreference => {
                            format!("Unknown preference \"{name}\"")
                        }
                    });
                };
            }
        }

        Ok(())
    }

    pub async fn find_server(&self) -> Option<SocketAddr> {
        trace!("preferences are {:#?}", self.preferences);

        let (must_match_tags, must_not_match_tags) = self.preferences.build_filter_tags();

        let mut best_server: Option<(&ServerInfo, f32)> = None;

        let server_list = self.global.server_list().await;
        for server in server_list.iter() {
            if !self.filter_server(server, must_match_tags, must_not_match_tags) {
                continue;
            }

            let current_server_score = self.recalculate_server_score(server);
            if current_server_score <= 1.0 {
                trace!(
                    "filtered server: {:?}, due to bad score {}",
                    server,
                    current_server_score
                );
                continue;
            }

            if let Some((_, best_server_score)) = best_server {
                if current_server_score > best_server_score {
                    best_server = Some((server, current_server_score));
                }
            } else {
                best_server = Some((server, current_server_score));
            }
        }

        best_server.map(|(s, _)| s.addr)
    }

    fn filter_server(
        &self,
        server: &ServerInfo,
        must_match_tags: ServerTags,
        must_not_match_tags: ServerTags,
    ) -> bool {
        // TODO: filter by player count
        // TODO: filter by gamemodes
        // TODO: server bans?

        if (server.tags & must_match_tags) != must_match_tags {
            trace!(
                "filtered server: {:?}, due to must_match_tags {:?}",
                server,
                must_match_tags
            );

            return false;
        }
        if !(server.tags & must_not_match_tags).is_empty() {
            trace!(
                "filtered server: {:?}, due to must_not_match_tags {:?}",
                server,
                server.tags & must_not_match_tags
            );

            return false;
        }

        for banned_map in self.preferences.map_bans.iter().flatten() {
            if server.map == *banned_map {
                trace!(
                    "filtered server: {:?}, due to banned map {}",
                    server,
                    banned_map
                );
                return false;
            }
        }

        true
    }

    fn recalculate_server_score(&self, server: &ServerInfo) -> f32 {
        server.score + self.score_server_for_user(server) + self.score_server(server)
    }

    fn score_server_for_user(&self, _server: &ServerInfo) -> f32 {
        // TODO: recalculate ping based on user location
        // TODO: since this isn't getting the real ping, no point in having it around
        // TODO: score servers by user ping
        let score: f32 = 0.0;

        score
    }

    fn score_server(&self, server: &ServerInfo) -> f32 {
        if self.preferences.party_size <= 1 {
            return 0.0;
        }

        let default_score = Self::score_server_by_players(server.players, server.max_players, 1);
        let new_score = Self::score_server_by_players(
            server.players,
            server.max_players,
            self.preferences.party_size,
        );

        new_score - default_score
    }

    fn score_server_by_players(players: u8, max_players: u8, party_size: u8) -> f32 {
        // Convert everything to f32 to make calculations easier
        let (players, max_players, party_size): (f32, f32, f32) =
            (players.into(), max_players.into(), party_size.into());

        let new_player_count = players + party_size;

        if (new_player_count + f32::from(SERVER_HEADROOM)) > max_players {
            return -100.0;
        }

        let mut new_max_players = max_players;
        if max_players > FULL_PLAYERS.into() {
            new_max_players = max_players - f32::from(FULL_PLAYERS);
        }

        if players == 0.0 {
            return -0.3;
        }

        let count_low = to_nearest_even(max_players / 3.0);
        let count_ideal = to_nearest_even(max_players * 0.72);

        const SCORE_LOW: f32 = 0.1;
        const SCORE_IDEAL: f32 = 1.6;
        const SCORE_FULLER: f32 = 0.2;

        if new_player_count <= count_low {
            lerp(0.0, count_low, 0.0, SCORE_LOW, new_player_count)
        } else if new_player_count <= count_ideal {
            return lerp(
                count_low,
                count_ideal,
                SCORE_LOW,
                SCORE_IDEAL,
                new_player_count,
            );
        } else if new_player_count <= new_max_players {
            return lerp(
                count_ideal,
                new_max_players,
                SCORE_IDEAL,
                SCORE_FULLER,
                new_player_count,
            );
        } else {
            return lerp(
                new_max_players,
                max_players,
                SCORE_FULLER,
                SCORE_LOW,
                new_player_count,
            );
        }
    }
}

fn decode_enum_preference<P: TryFromPrimitive<Primitive = u8>>(
    value: &str,
) -> Result<P, PreferenceDecodeError> {
    let value = value
        .parse::<u8>()
        .map_err(|_| PreferenceDecodeError::UnparseableValue)?;

    P::try_from_primitive(value).map_err(|_| PreferenceDecodeError::InvalidValue)
}

fn lerp(in_a: f32, in_b: f32, out_a: f32, out_b: f32, x: f32) -> f32 {
    out_a + ((out_b - out_a) * (x - in_a)) / (in_b - in_a)
}

fn to_nearest_even(num: f32) -> f32 {
    2.0 * (num / 2.0).round()
}
