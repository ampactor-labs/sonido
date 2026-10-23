//! The host side of the rig link ([`crate::link`]): talk to a pedal over any
//! byte stream, normally its USB serial port.
//!
//! [`LinkClient`] sends one request and waits for its reply over any
//! `Read + Write` stream: [`open_pedal`] finds and opens the pedal's serial
//! port (feature `serial`), and tests hand it an in-memory pedal. The
//! protocol's rules sit outside it ([`reply`], [`slot_rig`], [`slot_sector`],
//! [`choose_send_slot`], the timeouts) so the browser editor's Web Serial
//! path, which can't block, plays by the same ones.

use std::fmt;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use crate::link::{ErrorCode, FrameDecoder, Info, Reply, Request, Status, frame};
use crate::pedal::RigRef;
use crate::{Patch, PatchError, SECTOR_SIZE, decode, encode};

/// How long most requests may take.
pub const TIMEOUT: Duration = Duration::from_secs(2);
/// A flash write: sector erase (up to 300 ms), programming, read-back.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// A reply frame as a result: an error reply becomes [`LinkError::Refused`],
/// an unknown kind [`LinkError::BadReply`].
pub fn reply(kind: u8, arg: u8, payload: &[u8]) -> Result<(Reply, u8, Vec<u8>), LinkError> {
    match Reply::from_byte(kind) {
        Some(Reply::Error) => Err(LinkError::Refused(
            ErrorCode::from_byte(arg).unwrap_or(ErrorCode::Unsupported),
        )),
        Some(reply) => Ok((reply, arg, payload.to_vec())),
        None => Err(LinkError::BadReply),
    }
}

/// A bank sector as its rig: `None` for an empty (erased) slot.
pub fn slot_rig(sector: &[u8]) -> Result<Option<Patch>, LinkError> {
    if sector.iter().all(|&b| b == 0xFF) {
        return Ok(None);
    }
    decode(sector).map(Some).map_err(LinkError::Patch)
}

/// `rig` as the sector [`Request::WriteSlot`] carries (erased-flash padded).
pub fn slot_sector(rig: &Patch) -> Result<Vec<u8>, LinkError> {
    let mut sector = vec![0xFFu8; SECTOR_SIZE];
    encode(rig, &mut sector).map_err(LinkError::Patch)?;
    Ok(sector)
}

/// What can go wrong talking to a pedal.
#[derive(Debug)]
pub enum LinkError {
    /// The port failed.
    Io(io::Error),
    /// No reply in time: wrong port, or the pedal is busy or not running the
    /// Sonido firmware.
    Timeout,
    /// The pedal refused the request.
    Refused(ErrorCode),
    /// A reply that makes no sense for the request.
    BadReply,
    /// A rig that does not fit a sector, or a sector that is not a rig.
    Patch(PatchError),
    /// Every bank slot holds a rig.
    BankFull,
    /// No Sonido pedal on USB.
    NotFound,
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "serial port: {e}"),
            Self::Timeout => write!(f, "the pedal did not answer"),
            Self::Refused(code) => write!(f, "the pedal refused: {}", code.describe()),
            Self::BadReply => write!(f, "the pedal sent an unexpected reply"),
            Self::Patch(e) => write!(f, "{e}"),
            Self::BankFull => write!(f, "every bank slot holds a rig: erase one first"),
            Self::NotFound => write!(
                f,
                "no Sonido pedal found: connect the Seed's USB port while the pedal is on"
            ),
        }
    }
}

impl std::error::Error for LinkError {}

impl From<io::Error> for LinkError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// A connection to a pedal.
pub struct LinkClient<T> {
    io: T,
    decoder: FrameDecoder,
}

impl<T: Read + Write> LinkClient<T> {
    /// Talk to a pedal over `io` (reads may time out; that is retried until
    /// the request's own deadline).
    pub fn new(io: T) -> Self {
        Self {
            io,
            decoder: FrameDecoder::new(),
        }
    }

    /// What the pedal is: protocol, slots, factory rigs, effects, firmware.
    pub fn info(&mut self) -> Result<Info, LinkError> {
        let (reply, _, payload) = self.request(Request::Hello, 0, &[], TIMEOUT)?;
        match reply {
            Reply::Info => Info::decode(&payload).ok_or(LinkError::BadReply),
            _ => Err(LinkError::BadReply),
        }
    }

    /// What the pedal is doing: active rig, bypass, morph, CPU load.
    pub fn status(&mut self) -> Result<Status, LinkError> {
        let (reply, _, payload) = self.request(Request::Status, 0, &[], TIMEOUT)?;
        match reply {
            Reply::Status => Status::decode(&payload).ok_or(LinkError::BadReply),
            _ => Err(LinkError::BadReply),
        }
    }

    /// The rig in bank `slot` (0-based), or `None` if the slot is empty.
    pub fn read_slot(&mut self, slot: u8) -> Result<Option<Patch>, LinkError> {
        slot_rig(&self.sector(Request::ReadSlot, slot)?)
    }

    /// The jam as the pedal plays it now, as a rig.
    pub fn read_jam(&mut self) -> Result<Patch, LinkError> {
        let sector = self.sector(Request::ReadJam, 0)?;
        decode(&sector).map_err(LinkError::Patch)
    }

    /// Store `rig` in bank `slot` (0-based). The pedal checks it first.
    pub fn write_slot(&mut self, slot: u8, rig: &Patch) -> Result<(), LinkError> {
        self.expect_ok(Request::WriteSlot, slot, &slot_sector(rig)?, WRITE_TIMEOUT)
    }

    /// Each bank slot's rig name, in slot order: `None` for an empty slot or
    /// one that doesn't decode (which [`send_rig`](Self::send_rig) may
    /// overwrite).
    pub fn bank(&mut self) -> Result<Vec<Option<String>>, LinkError> {
        let slots = self.info()?.slot_count;
        (0..slots)
            .map(|slot| match self.read_slot(slot) {
                Ok(rig) => Ok(rig.map(|r| r.name)),
                Err(LinkError::Patch(_)) => Ok(None),
                Err(e) => Err(e),
            })
            .collect()
    }

    /// Store `rig` the way the editor and CLI send rigs ([`choose_send_slot`]).
    /// Returns the 0-based slot; the pedal plays it.
    pub fn send_rig(&mut self, rig: &Patch) -> Result<u8, LinkError> {
        let bank = self.bank()?;
        let slot = choose_send_slot(&bank, &rig.name).ok_or(LinkError::BankFull)?;
        self.write_slot(slot, rig)?;
        Ok(slot)
    }

    /// Empty bank `slot` (0-based).
    pub fn erase_slot(&mut self, slot: u8) -> Result<(), LinkError> {
        self.expect_ok(Request::EraseSlot, slot, &[], WRITE_TIMEOUT)
    }

    /// Play `rig`.
    pub fn activate(&mut self, rig: RigRef) -> Result<(), LinkError> {
        self.expect_ok(Request::Activate, rig.to_byte(), &[], TIMEOUT)
    }

    /// Restart the pedal into its bootloader for a firmware update. The
    /// serial port goes away; the pedal comes back as a USB DFU device.
    pub fn enter_bootloader(&mut self) -> Result<(), LinkError> {
        self.expect_ok(Request::Bootloader, 0, &[], TIMEOUT)
    }

    fn sector(&mut self, request: Request, arg: u8) -> Result<Vec<u8>, LinkError> {
        match self.request(request, arg, &[], TIMEOUT)? {
            (Reply::Sector, _, payload) if payload.len() == SECTOR_SIZE => Ok(payload),
            _ => Err(LinkError::BadReply),
        }
    }

    fn expect_ok(
        &mut self,
        request: Request,
        arg: u8,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<(), LinkError> {
        match self.request(request, arg, payload, timeout)? {
            (Reply::Ok, _, _) => Ok(()),
            _ => Err(LinkError::BadReply),
        }
    }

    /// Send one request and wait for its reply; an error reply becomes
    /// [`LinkError::Refused`].
    fn request(
        &mut self,
        request: Request,
        arg: u8,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<(Reply, u8, Vec<u8>), LinkError> {
        self.io.write_all(&frame(request as u8, arg, payload))?;
        self.io.flush()?;
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 512];
        loop {
            if Instant::now() > deadline {
                return Err(LinkError::Timeout);
            }
            let n = match self.io.read(&mut buf) {
                Ok(n) => n,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let mut reply = None;
            self.decoder.feed(&buf[..n], |f| {
                if reply.is_none() {
                    reply = Some((f.kind, f.arg, f.payload.to_vec()));
                }
            });
            if let Some((kind, arg, payload)) = reply {
                return self::reply(kind, arg, &payload);
            }
        }
    }
}

/// Where a rig named `name` goes in a bank whose slots hold `bank` (as
/// [`LinkClient::bank`] reads it): over the bank rig with that name, so
/// sending again after an edit replaces it, else into the first empty slot.
/// `None` when the bank is full.
pub fn choose_send_slot(bank: &[Option<String>], name: &str) -> Option<u8> {
    let slot = bank
        .iter()
        .position(|held| held.as_deref() == Some(name))
        .or_else(|| bank.iter().position(Option::is_none))?;
    u8::try_from(slot).ok()
}

/// The pedal's USB ids (pid.codes test VID/PID; product "Sonido Pedal").
pub const USB_VID: u16 = 0x1209;
/// See [`USB_VID`].
pub const USB_PID: u16 = 0x0001;

/// Serial ports that look like a Sonido pedal.
#[cfg(feature = "serial")]
pub fn pedal_ports() -> Vec<String> {
    use serialport::SerialPortType;
    serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| {
            matches!(&p.port_type, SerialPortType::UsbPort(u) if u.vid == USB_VID && u.pid == USB_PID)
        })
        .map(|p| p.port_name)
        .collect()
}

/// Open the pedal on `port`, or the first one found.
#[cfg(feature = "serial")]
pub fn open_pedal(
    port: Option<&str>,
) -> Result<LinkClient<Box<dyn serialport::SerialPort>>, LinkError> {
    let name = match port {
        Some(p) => p.to_owned(),
        None => pedal_ports()
            .into_iter()
            .next()
            .ok_or(LinkError::NotFound)?,
    };
    let port = serialport::new(&name, 115_200)
        .timeout(Duration::from_millis(50))
        .open()
        .map_err(|e| LinkError::Io(e.into()))?;
    Ok(LinkClient::new(port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Patch, PatchNode};
    use std::collections::VecDeque;

    /// An in-memory pedal: a bank of sectors, answering the way the firmware
    /// does.
    struct FakePedal {
        bank: Vec<Vec<u8>>,
        active: RigRef,
        decoder: FrameDecoder,
        outbox: VecDeque<u8>,
    }

    impl FakePedal {
        fn new() -> Self {
            Self {
                bank: vec![vec![0xFF; SECTOR_SIZE]; 15],
                active: RigRef::Jam,
                decoder: FrameDecoder::new(),
                outbox: VecDeque::new(),
            }
        }

        fn answer(&mut self, kind: u8, arg: u8, payload: &[u8]) -> Vec<u8> {
            let refuse = |code: ErrorCode| frame(Reply::Error as u8, code as u8, &[]);
            let slot = arg as usize;
            match Request::from_byte(kind) {
                Some(Request::Hello) => {
                    let info = Info {
                        protocol: crate::link::PROTOCOL_VERSION,
                        slot_count: 15,
                        factory_count: 6,
                        active: self.active,
                        effects: vec![1, 11],
                        firmware: "fake".into(),
                    };
                    frame(Reply::Info as u8, 0, &info.encode())
                }
                Some(Request::ReadSlot) if slot < 15 => {
                    frame(Reply::Sector as u8, arg, &self.bank[slot])
                }
                Some(Request::WriteSlot) if slot < 15 => {
                    if decode(payload).is_err() {
                        return refuse(ErrorCode::NotARig);
                    }
                    self.bank[slot] = payload.to_vec();
                    frame(Reply::Ok as u8, 0, &[])
                }
                Some(Request::EraseSlot) if slot < 15 => {
                    self.bank[slot] = vec![0xFF; SECTOR_SIZE];
                    frame(Reply::Ok as u8, 0, &[])
                }
                Some(Request::Activate) => {
                    self.active = RigRef::from_byte(arg);
                    frame(Reply::Ok as u8, 0, &[])
                }
                Some(_) => refuse(ErrorCode::BadSlot),
                None => refuse(ErrorCode::Unsupported),
            }
        }
    }

    impl Write for FakePedal {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut requests = Vec::new();
            self.decoder.feed(bytes, |f| {
                requests.push((f.kind, f.arg, f.payload.to_vec()));
            });
            for (kind, arg, payload) in requests {
                let reply = self.answer(kind, arg, &payload);
                self.outbox.extend(reply);
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Read for FakePedal {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            // A serial port hands over what has arrived, in pieces.
            let n = buf.len().min(self.outbox.len()).min(61);
            if n == 0 {
                return Err(io::ErrorKind::TimedOut.into());
            }
            for b in buf.iter_mut().take(n) {
                *b = self.outbox.pop_front().unwrap();
            }
            Ok(n)
        }
    }

    fn rig() -> Patch {
        Patch::linear_chain("Sent", vec![PatchNode::new(1, vec![20.0])])
    }

    #[test]
    fn hello_reports_the_pedal() {
        let mut link = LinkClient::new(FakePedal::new());
        let info = link.info().unwrap();
        assert_eq!((info.slot_count, info.factory_count), (15, 6));
        assert_eq!(info.firmware, "fake");
    }

    #[test]
    fn a_sent_rig_reads_back_and_erases() {
        let mut link = LinkClient::new(FakePedal::new());
        assert_eq!(link.read_slot(2).unwrap(), None);
        link.write_slot(2, &rig()).unwrap();
        let back = link.read_slot(2).unwrap().expect("stored");
        assert_eq!(back.name, "Sent");
        assert_eq!(back.nodes.len(), 1);
        link.erase_slot(2).unwrap();
        assert_eq!(link.read_slot(2).unwrap(), None);
    }

    #[test]
    fn sending_again_replaces_the_rig_with_that_name() {
        let mut link = LinkClient::new(FakePedal::new());
        let other = Patch::linear_chain("Other", vec![PatchNode::new(1, vec![5.0])]);
        assert_eq!(link.send_rig(&other).unwrap(), 0);
        assert_eq!(link.send_rig(&rig()).unwrap(), 1);
        let mut edited = rig();
        edited.nodes[0].params_a[0] = 30.0;
        assert_eq!(link.send_rig(&edited).unwrap(), 1, "same name, same slot");
        let back = link.read_slot(1).unwrap().unwrap();
        assert_eq!(back.nodes[0].params_a[0], 30.0);
        assert_eq!(link.read_slot(2).unwrap(), None);
    }

    #[test]
    fn a_rig_replaces_its_name_else_takes_the_first_empty_slot() {
        let bank = [
            Some("Lead".to_owned()),
            None,
            Some("Clean".to_owned()),
            None,
        ];
        assert_eq!(choose_send_slot(&bank, "Clean"), Some(2));
        assert_eq!(choose_send_slot(&bank, "New"), Some(1));
        let full = [Some("A".to_owned()), Some("B".to_owned())];
        assert_eq!(choose_send_slot(&full, "C"), None);
        assert_eq!(choose_send_slot(&full, "B"), Some(1));
    }

    #[test]
    fn refusals_carry_the_pedals_reason() {
        let mut link = LinkClient::new(FakePedal::new());
        match link.read_slot(40) {
            Err(LinkError::Refused(ErrorCode::BadSlot)) => {}
            other => panic!("expected a bad-slot refusal, got {other:?}"),
        }
        link.activate(RigRef::Factory(2)).unwrap();
        assert_eq!(link.info().unwrap().active, RigRef::Factory(2));
    }
}
