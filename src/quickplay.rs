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
//         max 255 convars can be sent, minus existing

pub struct QuickplaySession {}

impl QuickplaySession {
    pub fn new() -> Self {
        Self {}
    }

    pub fn update_settings_from_convars(&mut self, _convars: &[(String, String)]) {
        // TODO
    }

    pub fn find_match(&self) -> Option<String> {
        // TODO
        None
    }
}
