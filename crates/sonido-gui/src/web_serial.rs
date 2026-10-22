//! Send a rig to the pedal from the browser editor, over Web Serial (Chrome
//! and Edge; Firefox and Safari don't have it).
//!
//! It speaks the link protocol the native client does, by the same rules:
//! frames from `link::frame`, replies through `link::FrameDecoder` and
//! `client::reply`, sectors through `client::slot_rig` and
//! `client::slot_sector`, the slot from `client::choose_send_slot`, and the
//! client's timeouts. It can't reuse `LinkClient`, which blocks and reads
//! `std::time::Instant` (neither works in a browser); here each read is a
//! promise raced against a timer.

use std::time::Duration;

use js_sys::{Array, Date, Promise, Uint8Array};
use sonido_patch::client::{
    self, LinkError, TIMEOUT, USB_PID, USB_VID, WRITE_TIMEOUT, choose_send_slot,
};
use sonido_patch::link::{FrameDecoder, Info, Reply, Request, frame};
use sonido_patch::{Patch, SECTOR_SIZE};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    ReadableStreamDefaultReader, ReadableStreamReadResult, SerialOptions, SerialPort,
    SerialPortFilter, SerialPortRequestOptions, WritableStreamDefaultWriter,
};

/// Where a sent rig went.
pub struct Sent {
    /// The bank slot (0-based); the pedal plays it.
    pub slot: u8,
    /// The firmware's link protocol (see `link::unplayed`).
    pub protocol: u8,
}

/// Ask the browser for the pedal's port, then store `rig` the way the
/// native editor does: over the bank rig with its name, else in the first
/// empty slot. The pedal plays it.
///
/// # Errors
///
/// A message for the user: no Web Serial, no pedal picked, or the link's
/// own errors.
pub async fn send_rig(rig: &Patch) -> Result<Sent, String> {
    let port = pick_port().await?;
    JsFuture::from(port.open(&SerialOptions::new(115_200)))
        .await
        .map_err(|e| format!("Couldn't open the pedal's port: {}", js_message(&e)))?;
    let mut link = WebLink::new(port)?;
    let sent = link.send(rig).await;
    link.close().await;
    sent
}

/// The browser's port picker, showing only the pedal.
async fn pick_port() -> Result<SerialPort, String> {
    let window = web_sys::window().ok_or("no browser window")?;
    let serial = window.navigator().serial();
    if serial.is_undefined() {
        return Err(
            "Sending to the pedal needs Web Serial: open the editor in Chrome or Edge, or use \
             the desktop app"
                .to_owned(),
        );
    }
    let filter = SerialPortFilter::new();
    filter.set_usb_vendor_id(USB_VID);
    filter.set_usb_product_id(USB_PID);
    let options = SerialPortRequestOptions::new();
    options.set_filters(&[filter]);
    JsFuture::from(serial.request_port_with_options(&options))
        .await
        .map_err(|e| {
            if e.dyn_ref::<js_sys::Error>()
                .is_some_and(|e| e.name() == "NotFoundError")
            {
                "No pedal picked".to_owned()
            } else {
                js_message(&e)
            }
        })
}

/// An open port with its stream locks.
struct WebLink {
    port: SerialPort,
    reader: ReadableStreamDefaultReader,
    writer: WritableStreamDefaultWriter,
    decoder: FrameDecoder,
    /// A read still unanswered when a request timed out: a reader allows one
    /// read at a time, so the next request awaits this one.
    pending: Option<Promise>,
}

impl WebLink {
    fn new(port: SerialPort) -> Result<Self, String> {
        let reader =
            ReadableStreamDefaultReader::new(&port.readable()).map_err(|e| js_message(&e))?;
        let writer = port.writable().get_writer().map_err(|e| js_message(&e))?;
        Ok(Self {
            port,
            reader,
            writer,
            decoder: FrameDecoder::new(),
            pending: None,
        })
    }

    /// Hello, read the bank's names, choose the slot, write it.
    async fn send(&mut self, rig: &Patch) -> Result<Sent, String> {
        let info = match self.request(Request::Hello, 0, &[], TIMEOUT).await? {
            (Reply::Info, _, payload) => Info::decode(&payload).ok_or(LinkError::BadReply),
            _ => Err(LinkError::BadReply),
        }
        .map_err(|e| e.to_string())?;
        let mut bank = Vec::with_capacity(usize::from(info.slot_count));
        for slot in 0..info.slot_count {
            let sector = match self.request(Request::ReadSlot, slot, &[], TIMEOUT).await? {
                (Reply::Sector, _, payload) if payload.len() == SECTOR_SIZE => payload,
                _ => return Err(LinkError::BadReply.to_string()),
            };
            bank.push(match client::slot_rig(&sector) {
                Ok(held) => held.map(|r| r.name),
                Err(LinkError::Patch(_)) => None,
                Err(e) => return Err(e.to_string()),
            });
        }
        let slot = choose_send_slot(&bank, &rig.name).ok_or(LinkError::BankFull.to_string())?;
        let sector = client::slot_sector(rig).map_err(|e| e.to_string())?;
        match self
            .request(Request::WriteSlot, slot, &sector, WRITE_TIMEOUT)
            .await?
        {
            (Reply::Ok, _, _) => Ok(Sent {
                slot,
                protocol: info.protocol,
            }),
            _ => Err(LinkError::BadReply.to_string()),
        }
    }

    /// Send one request and wait up to `timeout` for its reply.
    async fn request(
        &mut self,
        request: Request,
        arg: u8,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<(Reply, u8, Vec<u8>), String> {
        let bytes = Uint8Array::new_from_slice(&frame(request as u8, arg, payload));
        JsFuture::from(self.writer.write_with_chunk(&bytes))
            .await
            .map_err(|e| format!("serial port: {}", js_message(&e)))?;
        let deadline = Date::now() + timeout.as_secs_f64() * 1000.0;
        loop {
            let left = deadline - Date::now();
            if left <= 0.0 {
                return Err(LinkError::Timeout.to_string());
            }
            let read = self.pending.take().unwrap_or_else(|| self.reader.read());
            let raced = JsFuture::from(Promise::race(&Array::of2(&read, &after(left))))
                .await
                .map_err(|e| format!("serial port: {}", js_message(&e)))?;
            // The timer resolves with `undefined`; a read with a result object.
            if raced.is_undefined() {
                self.pending = Some(read);
                return Err(LinkError::Timeout.to_string());
            }
            let result: ReadableStreamReadResult = raced.unchecked_into();
            if result.get_done() == Some(true) {
                return Err("the pedal's port closed".to_owned());
            }
            let chunk = Uint8Array::new(&result.get_value()).to_vec();
            let mut reply = None;
            self.decoder.feed(&chunk, |f| {
                if reply.is_none() {
                    reply = Some((f.kind, f.arg, f.payload.to_vec()));
                }
            });
            if let Some((kind, arg, payload)) = reply {
                return client::reply(kind, arg, &payload).map_err(|e| e.to_string());
            }
        }
    }

    /// Let go of the streams and close the port (best effort: the port goes
    /// away with the page anyway).
    async fn close(self) {
        let _ = JsFuture::from(self.reader.cancel()).await;
        self.reader.release_lock();
        self.writer.release_lock();
        let _ = JsFuture::from(self.port.close()).await;
    }
}

/// A promise that resolves with `undefined` after `ms` milliseconds.
fn after(ms: f64) -> Promise {
    Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ = window
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms.ceil() as i32);
        }
    })
}

/// A JavaScript error's message, for people.
fn js_message(e: &JsValue) -> String {
    e.dyn_ref::<js_sys::Error>()
        .map(|e| String::from(e.message()))
        .or_else(|| e.as_string())
        .unwrap_or_else(|| format!("{e:?}"))
}
