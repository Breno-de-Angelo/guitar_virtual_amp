//! MIDI input for footswitches and controllers (desktop only; on Android
//! every call is a no-op that reports no ports). Messages are parsed on
//! midir's thread (`shared::midi::parse_message`) and queued for the UI,
//! which maps them to actions through the user's bindings.

use crossbeam::channel::{Receiver, Sender, unbounded};

use crate::shared::midi::MidiTrigger;

pub struct MidiInputManager {
    tx: Sender<(MidiTrigger, u8)>,
    rx: Receiver<(MidiTrigger, u8)>,
    #[cfg(not(target_os = "android"))]
    connection: Option<midir::MidiInputConnection<()>>,
    port: Option<String>,
    error: Option<String>,
}

impl Default for MidiInputManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiInputManager {
    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        Self {
            tx,
            rx,
            #[cfg(not(target_os = "android"))]
            connection: None,
            port: None,
            error: None,
        }
    }

    /// Names of the MIDI input ports available now.
    pub fn list_ports() -> Result<Vec<String>, String> {
        #[cfg(not(target_os = "android"))]
        {
            let input = midir::MidiInput::new("Guitar Virtual Amp (list)").map_err(|e| e.to_string())?;
            Ok(input.ports().iter().filter_map(|p| input.port_name(p).ok()).collect())
        }
        #[cfg(target_os = "android")]
        {
            Ok(Vec::new())
        }
    }

    /// Opens `port_name`, closing any previous connection.
    pub fn connect(&mut self, port_name: &str) -> Result<(), String> {
        self.disconnect();
        #[cfg(not(target_os = "android"))]
        {
            let result = (|| {
                let mut input = midir::MidiInput::new("Guitar Virtual Amp").map_err(|e| e.to_string())?;
                input.ignore(midir::Ignore::All);
                let port = input
                    .ports()
                    .into_iter()
                    .find(|p| input.port_name(p).is_ok_and(|n| n == port_name))
                    .ok_or_else(|| format!("MIDI port \"{port_name}\" not found"))?;
                let tx = self.tx.clone();
                input
                    .connect(
                        &port,
                        "gva-in",
                        move |_stamp, message, _| {
                            if let Some(event) = crate::shared::midi::parse_message(message) {
                                let _ = tx.send(event);
                            }
                        },
                        (),
                    )
                    .map_err(|e| e.to_string())
            })();
            match result {
                Ok(connection) => {
                    self.connection = Some(connection);
                    self.port = Some(port_name.to_string());
                    self.error = None;
                    Ok(())
                }
                Err(e) => {
                    self.error = Some(e.clone());
                    Err(e)
                }
            }
        }
        #[cfg(target_os = "android")]
        {
            let _ = port_name;
            Err("MIDI is not supported on Android yet".into())
        }
    }

    pub fn disconnect(&mut self) {
        #[cfg(not(target_os = "android"))]
        if let Some(connection) = self.connection.take() {
            connection.close();
        }
        self.port = None;
    }

    pub fn port(&self) -> Option<&str> {
        self.port.as_deref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Messages received since the last call.
    pub fn poll(&self) -> Vec<(MidiTrigger, u8)> {
        self.rx.try_iter().collect()
    }

    /// Queues a message as if it came from the device (tests, on-screen pads).
    pub fn inject(&self, trigger: MidiTrigger, value: u8) {
        let _ = self.tx.send((trigger, value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_messages_are_polled_once() {
        let midi = MidiInputManager::new();
        midi.inject(MidiTrigger::ProgramChange { channel: 0 }, 3);
        assert_eq!(midi.poll(), vec![(MidiTrigger::ProgramChange { channel: 0 }, 3)]);
        assert!(midi.poll().is_empty());
        assert!(midi.port().is_none());
    }
}
