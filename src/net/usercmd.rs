use crate::io::bitstream::{BitReader, BitStreamError};

/// A single UserCmd. UserCmds are delta-compressed, so all of these are
/// Option<_>.
#[derive(Debug, PartialEq)]
pub struct UserCmd {
    pub command_nr: Option<u32>,
    pub tick_count: Option<u32>,
    pub viewangles: [Option<f32>; 3],
    pub forward_move: Option<f32>,
    pub side_move: Option<f32>,
    pub up_move: Option<f32>,
    pub buttons: Option<u32>,
    pub impulse: Option<u8>,
    pub weapon_select: Option<WeaponSelect>,
    pub mouse_dx: Option<u16>,
    pub mouse_dy: Option<u16>,
}

#[derive(Debug, PartialEq)]
pub struct WeaponSelect {
    pub weapon: u16,
    pub subtype: Option<u8>,
}

/// Read an optional field. The first bit in the stream controls if the field
/// exists or not.
macro_rules! read_optional {
    ($reader:ident, $fill:expr) => {
        if $reader.read_bit()? {
            Some($fill)
        } else {
            None
        }
    };
}

/// TODO: move this out of here
const MAX_EDICT_BITS: u8 = 11;
const WEAPON_SUBTYPE_BITS: u8 = 6;

impl UserCmd {
    /// Read a single UserCmd
    pub fn read(reader: &mut BitReader) -> Result<Self, BitStreamError> {
        let command_nr = read_optional!(reader, reader.read_in::<32, u32>()?);
        let tick_count = read_optional!(reader, reader.read_in::<32, u32>()?);

        let mut viewangles = [None; 3];
        for angle in &mut viewangles {
            *angle = read_optional!(reader, f32::from_bits(reader.read_in::<32, u32>()?));
        }

        let forward_move = read_optional!(reader, f32::from_bits(reader.read_in::<32, u32>()?));
        let side_move = read_optional!(reader, f32::from_bits(reader.read_in::<32, u32>()?));
        let up_move = read_optional!(reader, f32::from_bits(reader.read_in::<32, u32>()?));

        let buttons = read_optional!(reader, reader.read_in::<32, u32>()?);
        let impulse = read_optional!(reader, reader.read_in::<8, u8>()?);

        let weapon_select = read_optional!(reader, {
            let weapon = reader.read_in::<MAX_EDICT_BITS, u16>()?;
            let subtype = read_optional!(reader, reader.read_in::<WEAPON_SUBTYPE_BITS, u8>()?);
            WeaponSelect { weapon, subtype }
        });

        let mouse_dx = read_optional!(reader, reader.read_in::<16, u16>()?);
        let mouse_dy = read_optional!(reader, reader.read_in::<16, u16>()?);

        Ok(Self {
            command_nr,
            tick_count,
            viewangles,
            forward_move,
            side_move,
            up_move,
            buttons,
            impulse,
            weapon_select,
            mouse_dx,
            mouse_dy,
        })
    }
}
