use anyhow::Result;
use libghostty_vt::{
    Terminal,
    mouse::{Action, Button, Encoder, EncoderSize, Event, Position},
};
use swarm_term::Size;

pub(crate) struct WheelEncoder {
    encoder: Encoder<'static>,
    event: Event<'static>,
}

impl WheelEncoder {
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            encoder: Encoder::new()?,
            event: Event::new()?,
        })
    }

    /// One wheel press over a cell, in the tracking mode and format the program asked
    /// for. Empty when that mode has no wheel events (X10).
    pub(crate) fn encode(
        &mut self,
        terminal: &Terminal<'_, '_>,
        size: Size,
        up: bool,
        column: u16,
        row: u16,
    ) -> Result<Vec<u8>> {
        let cell_width = u32::from(size.cell_width.max(1));
        let cell_height = u32::from(size.cell_height.max(1));

        // The encoder turns pixels into cells, so aim at the middle of the cell.
        let x = (f32::from(column) + 0.5) * f32::from(size.cell_width.max(1));
        let y = (f32::from(row) + 0.5) * f32::from(size.cell_height.max(1));

        self.encoder
            .set_options_from_terminal(terminal)
            .set_size(EncoderSize {
                screen_width: u32::from(size.cols) * cell_width,
                screen_height: u32::from(size.rows) * cell_height,
                cell_width,
                cell_height,
                padding_top: 0,
                padding_bottom: 0,
                padding_right: 0,
                padding_left: 0,
            });
        self.event
            .set_action(Action::Press)
            .set_button(Some(if up { Button::Four } else { Button::Five }))
            .set_position(Position { x, y });

        let mut bytes = Vec::with_capacity(16);
        self.encoder.encode_to_vec(&self.event, &mut bytes)?;

        Ok(bytes)
    }
}
