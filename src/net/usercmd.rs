use crate::io::bitstream::{BitReader, BitStreamError, BitWriter};

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

/// Write an optional field. The first bit in the stream signifies if the field
/// exists or not.
macro_rules! write_optional {
    ($writer:ident, $value:expr, $renamed:ident => $fill: expr) => {
        if let Some($renamed) = $value {
            $writer.write_bit(true)?;
            $fill
        } else {
            $writer.write_bit(false)?;
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

    pub fn write(&self, writer: &mut BitWriter) -> Result<(), BitStreamError> {
        write_optional!(
            writer,
            self.command_nr,
            command_nr => writer.write_out::<32, u32>(command_nr)?
        );

        write_optional!(
            writer,
            self.tick_count,
            tick_count => writer.write_out::<32, u32>(tick_count)?
        );

        for angle in &self.viewangles {
            write_optional!(
                writer,
                *angle,
                angle => writer.write_out::<32, _>(angle.to_bits())?
            );
        }

        write_optional!(writer, self.forward_move, forward_move => writer.write_out::<32, _>(forward_move.to_bits())?);
        write_optional!(writer, self.side_move, side_move => writer.write_out::<32, _>(side_move.to_bits())?);
        write_optional!(writer, self.up_move, up_move => writer.write_out::<32, _>(up_move.to_bits())?);

        write_optional!(writer, self.buttons, buttons => writer.write_out::<32, u32>(buttons)?);
        write_optional!(writer, self.impulse, impulse => writer.write_out::<8, u8>(impulse)?);

        write_optional!(writer, &self.weapon_select, weapon_select => {
            writer.write_out::<MAX_EDICT_BITS, u16>(weapon_select.weapon)?;
            write_optional!(writer, weapon_select.subtype, subtype => writer.write_out::<WEAPON_SUBTYPE_BITS, u8>(subtype)?);
        });

        write_optional!(writer, self.mouse_dx, mouse_dx => writer.write_out::<16, u16>(mouse_dx)?);
        write_optional!(writer, self.mouse_dy, mouse_dy => writer.write_out::<16, u16>(mouse_dy)?);

        Ok(())
    }
}
