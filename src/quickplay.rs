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

use std::net::SocketAddr;

use bitflags::bitflags;
use num_enum::TryFromPrimitive;
use tracing::trace;

use crate::server_list::{ServerInfo, ServerTags};

const CONVAR_PREFERENCE_PREFIX: &str = "rqp_";
const MAX_MAP_BANS: usize = 6;

const PING_LOW_SCORE: f32 = 0.9;
const PING_MIN: f32 = 24.0;

const PING_MED: f32 = 150.0;
const PING_MED_SCORE: f32 = 0.0;

const PING_HIGH: f32 = 300.0;
const PING_HIGH_SCORE: f32 = -1.0;

const MIN_PING: f32 = PING_MIN + 1.0;
const MAX_PING: f32 = PING_MED - 1.0;

const SERVER_HEADROOM: u16 = 1;
const FULL_PLAYERS: u16 = 24;

#[derive(Debug, TryFromPrimitive)]
#[repr(u8)]
enum RandomCritsPreference {
    Enabled = 0,
    Disabled,
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
    Enabled = 0,
    Disabled,
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
    Enabled = 0,
    Disabled,
    DontCare,
}

enum PreferenceDecodeError {
    UnparseableValue = 0,
    InvalidValue,
    UnknownPreference,
    PingPreferenceNotInRange,
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
pub struct QuickplaySession {
    // server_capacity: ...,
    random_crits: RandomCritsPreference,
    respawn_times: RespawnTimesPreference,
    rtd: RtdPreference,

    class_restrictions: ClassRestrictionsPreference,
    objectives: ObjectivesPreference,

    ping_preference: u8,
    party_size: u8,

    map_bans: [Option<String>; MAX_MAP_BANS],
    _gamemodes: Gamemodes,
}

impl QuickplaySession {
    pub fn new() -> Self {
        Self {
            // server_capacity: todo!(),
            random_crits: RandomCritsPreference::DontCare,
            respawn_times: RespawnTimesPreference::Default,
            rtd: RtdPreference::Disabled,
            class_restrictions: ClassRestrictionsPreference::None,
            objectives: ObjectivesPreference::Enabled,
            ping_preference: 50,
            party_size: 1,
            map_bans: std::array::from_fn(|_| None),
            // Everything except for alternative and arena
            _gamemodes: Gamemodes::all() ^ (Gamemodes::ALTERNATIVE | Gamemodes::ARENA),
        }
    }

    pub fn update_preferences_from_convars(
        &mut self,
        convars: &[(String, String)],
    ) -> Result<(), String> {
        for (name, value) in convars {
            if name.starts_with(CONVAR_PREFERENCE_PREFIX) {
                let name = &name[CONVAR_PREFERENCE_PREFIX.len()..];
                if let Err(err_type) = self.update_preference(name, value) {
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
                        PreferenceDecodeError::PingPreferenceNotInRange => format!(
                            "Ping preference {value} not within range, minimum {}, maximum {}",
                            MIN_PING, MAX_PING,
                        ),
                    });
                };
            }
        }

        Ok(())
    }

    fn update_preference(
        &mut self,
        name: &str,
        value: &String,
    ) -> Result<(), PreferenceDecodeError> {
        const MAP_BAN_PREF_NAME: &str = "map_ban_";
        if name.starts_with(MAP_BAN_PREF_NAME) {
            let slot_index_str = &name[MAP_BAN_PREF_NAME.len()..];
            let slot_index = usize::from_str_radix(slot_index_str, 10)
                .map_err(|_| PreferenceDecodeError::UnknownPreference)?;

            if slot_index >= self.map_bans.len() {
                return Err(PreferenceDecodeError::UnknownPreference);
            }

            self.map_bans[slot_index] = Some(value.clone());
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
                    self.party_size = u8::from_str_radix(&value, 10)
                        .map_err(|_| PreferenceDecodeError::UnparseableValue)?
                }
                "ping_preference" => {
                    let ping_preference = u8::from_str_radix(&value, 10)
                        .map_err(|_| PreferenceDecodeError::InvalidValue)?;

                    if f32::from(ping_preference) < MIN_PING
                        || f32::from(ping_preference) > MAX_PING
                    {
                        return Err(PreferenceDecodeError::PingPreferenceNotInRange);
                    }

                    self.ping_preference = ping_preference;
                }
                _ => return Err(PreferenceDecodeError::UnknownPreference),
            }
        };

        Ok(())
    }

    pub fn find_server(&self, server_list: &[ServerInfo]) -> Option<SocketAddr> {
        trace!("preferences are {:#?}", self);

        let (must_match_tags, must_not_match_tags) = self.build_filter_tags();

        let mut best_server: Option<(&ServerInfo, f32)> = None;

        for server in server_list {
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

        for banned_map in &self.map_bans {
            if let Some(banned_map) = banned_map {
                if server.map == *banned_map {
                    trace!(
                        "filtered server: {:?}, due to banned map {}",
                        server,
                        banned_map
                    );
                    return false;
                }
            }
        }

        true
    }

    fn recalculate_server_score(&self, server: &ServerInfo) -> f32 {
        server.score + self.score_server_for_user(server) + self.score_server(server)
    }

    fn score_server_for_user(&self, server: &ServerInfo) -> f32 {
        // TODO: recalculate ping based on user location
        let mut score: f32 = 0.0;
        let ping = server.ping;

        if ping <= PING_MIN {
            score += 1.0;
        } else if ping < self.ping_preference.into() {
            score += lerp(
                PING_MIN,
                self.ping_preference.into(),
                1.0,
                PING_LOW_SCORE,
                ping,
            );
        } else if ping < PING_MED {
            score += lerp(
                self.ping_preference.into(),
                PING_MED,
                PING_LOW_SCORE,
                PING_MED_SCORE,
                ping,
            );
        } else {
            score += lerp(PING_MED, PING_HIGH, PING_MED_SCORE, PING_HIGH_SCORE, ping);
        }

        score
    }

    fn score_server(&self, server: &ServerInfo) -> f32 {
        if self.party_size <= 1 {
            return 0.0;
        }

        let default_score = Self::score_server_by_players(server.players, server.max_players, 1);
        let new_score =
            Self::score_server_by_players(server.players, server.max_players, self.party_size);

        new_score - default_score
    }

    fn score_server_by_players(players: u8, max_players: u8, party_size: u8) -> f32 {
        let new_player_count = u16::from(players) + u16::from(party_size);

        if (new_player_count + SERVER_HEADROOM) > max_players.into() {
            return -100.0;
        }

        let mut new_max_players = u16::from(max_players);
        if u16::from(max_players) > FULL_PLAYERS {
            new_max_players = u16::from(max_players) - FULL_PLAYERS;
        }

        if players == 0 {
            return -0.3;
        }

        let count_low = to_nearest_even(f32::from(max_players) / 3.0);
        let count_ideal = to_nearest_even(f32::from(max_players) * 0.72);

        const SCORE_LOW: f32 = 0.1;
        const SCORE_IDEAL: f32 = 1.6;
        const SCORE_FULLER: f32 = 0.2;

        if f32::from(new_player_count) <= count_low {
            return lerp(0.0, count_low, 0.0, SCORE_LOW, new_player_count.into());
        } else if f32::from(new_player_count) <= count_ideal {
            return lerp(
                count_low,
                count_ideal,
                SCORE_LOW,
                SCORE_IDEAL,
                new_player_count.into(),
            );
        } else if new_player_count <= new_max_players {
            return lerp(
                count_ideal,
                new_max_players.into(),
                SCORE_IDEAL,
                SCORE_FULLER,
                new_player_count.into(),
            );
        } else {
            return lerp(
                new_max_players.into(),
                max_players.into(),
                SCORE_FULLER,
                SCORE_LOW,
                new_player_count.into(),
            );
        }
    }
}

fn decode_enum_preference<P: TryFromPrimitive<Primitive = u8>>(
    value: &str,
) -> Result<P, PreferenceDecodeError> {
    let value =
        u8::from_str_radix(value, 10).map_err(|_| PreferenceDecodeError::UnparseableValue)?;

    P::try_from_primitive(value).map_err(|_| PreferenceDecodeError::InvalidValue)
}

fn lerp(in_a: f32, in_b: f32, out_a: f32, out_b: f32, x: f32) -> f32 {
    out_a + ((out_b - out_a) * (x - in_a)) / (in_b - in_a)
}

fn to_nearest_even(num: f32) -> f32 {
    2.0 * (num / 2.0).round()
}
