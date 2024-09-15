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

use num_enum::TryFromPrimitive;

const CONVAR_PREFERENCE_PREFIX: &str = "rqp_";
const MAX_MAP_BANS: usize = 6;

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
}

#[derive(Debug)]
pub struct QuickplaySession {
    // server_capacity: ...,
    random_crits: RandomCritsPreference,
    respawn_times: RespawnTimesPreference,
    rtd: RtdPreference,

    class_restrictions: ClassRestrictionsPreference,
    objectives: ObjectivesPreference,

    // ping_preference: u8,
    party_size: u8,

    map_bans: [Option<String>; MAX_MAP_BANS],
    // gamemodes: ...,
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
            // ping_preference: todo!()
            party_size: 1,
            map_bans: std::array::from_fn(|_| None),
            // gamemodes: todo!(),
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
                _ => return Err(PreferenceDecodeError::UnknownPreference),
            }
        };

        Ok(())
    }

    pub fn find_match(&self) -> Option<String> {
        // TODO
        None
    }
}

fn decode_enum_preference<P: TryFromPrimitive<Primitive = u8>>(
    value: &str,
) -> Result<P, PreferenceDecodeError> {
    let value =
        u8::from_str_radix(value, 10).map_err(|_| PreferenceDecodeError::UnparseableValue)?;

    P::try_from_primitive(value).map_err(|_| PreferenceDecodeError::InvalidValue)
}
