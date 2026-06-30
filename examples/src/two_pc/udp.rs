use std::{
    collections::VecDeque,
    io,
    net::{SocketAddr, UdpSocket},
    time::{Duration, Instant},
};

use traceforge_rounds::{Envelope, Transport};

use super::{Msg, Phase, TwoPcRound};

const LOCALHOST: [u8; 4] = [127, 0, 0, 1];

#[derive(Debug)]
pub struct UdpTransport {
    socket: UdpSocket,
    recv_timeout: Duration,
    inbox_timeout: Duration,
    buffer: VecDeque<Envelope<TwoPcRound, Msg<u16>>>,
}

impl UdpTransport {
    pub fn bind(port: u16) -> io::Result<Self> {
        let socket = UdpSocket::bind(SocketAddr::from((LOCALHOST, port)))?;
        Ok(Self {
            socket,
            recv_timeout: Duration::from_millis(500),
            inbox_timeout: Duration::from_millis(500),
            buffer: VecDeque::new(),
        })
    }

    pub fn with_recv_timeout(mut self, timeout: Duration) -> Self {
        self.recv_timeout = timeout;
        self
    }

    pub fn with_inbox_timeout(mut self, timeout: Duration) -> Self {
        self.inbox_timeout = timeout;
        self
    }

    fn recv_matching<F>(
        &mut self,
        current: &TwoPcRound,
        filter: &F,
        timeout: Option<Duration>,
    ) -> io::Result<Option<Envelope<TwoPcRound, Msg<u16>>>>
    where
        F: Fn(&TwoPcRound, &TwoPcRound) -> bool,
    {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);

        loop {
            if let Some(pos) = self
                .buffer
                .iter()
                .position(|entry| filter(current, entry.stamp()))
            {
                return Ok(self.buffer.remove(pos));
            }

            if let Some(deadline) = deadline {
                let now = Instant::now();
                if now >= deadline {
                    return Ok(None);
                }
                self.socket.set_read_timeout(Some(deadline - now))?;
            } else {
                self.socket.set_read_timeout(None)?;
            }

            let mut bytes = [0_u8; 64];
            match self.socket.recv_from(&mut bytes) {
                Ok((len, _src)) => {
                    let envelope = decode_packet(&bytes[..len])?;
                    if filter(current, envelope.stamp()) {
                        return Ok(Some(envelope));
                    }
                    self.buffer.push_back(envelope);
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(err) => return Err(err),
            }
        }
    }
}

impl Transport<TwoPcRound, Msg<u16>> for UdpTransport {
    type Node = u16;
    type Error = io::Error;

    fn send(
        &mut self,
        dst: Self::Node,
        envelope: Envelope<TwoPcRound, Msg<u16>>,
    ) -> Result<(), Self::Error> {
        let bytes = encode_packet(envelope);
        self.socket
            .send_to(&bytes, SocketAddr::from((LOCALHOST, dst)))?;
        Ok(())
    }

    fn recv<F>(
        &mut self,
        current: &TwoPcRound,
        filter: F,
    ) -> Result<Option<Envelope<TwoPcRound, Msg<u16>>>, Self::Error>
    where
        F: Fn(&TwoPcRound, &TwoPcRound) -> bool + Send + Sync + 'static,
    {
        let timeout = self.recv_timeout;
        self.recv_matching(current, &filter, Some(timeout))
    }

    fn recv_block<F>(
        &mut self,
        current: &TwoPcRound,
        filter: F,
    ) -> Result<Envelope<TwoPcRound, Msg<u16>>, Self::Error>
    where
        F: Fn(&TwoPcRound, &TwoPcRound) -> bool + Send + Sync + 'static,
    {
        loop {
            if let Some(envelope) = self.recv_matching(current, &filter, None)? {
                return Ok(envelope);
            }
        }
    }

    fn inbox<F>(
        &mut self,
        current: &TwoPcRound,
        filter: F,
        min: usize,
        max: Option<usize>,
    ) -> Result<Vec<Option<Envelope<TwoPcRound, Msg<u16>>>>, Self::Error>
    where
        F: Fn(&TwoPcRound, &TwoPcRound) -> bool + Send + Sync + 'static,
    {
        let limit = max.unwrap_or(min.max(1));
        let mut entries = Vec::with_capacity(limit);
        let deadline = Instant::now() + self.inbox_timeout;

        while entries.len() < limit {
            let now = Instant::now();
            if now >= deadline {
                break;
            }

            match self.recv_matching(current, &filter, Some(deadline - now))? {
                Some(envelope) => entries.push(Some(envelope)),
                None => break,
            }
        }

        while entries.len() < min {
            if let Some(envelope) = self.recv_matching(current, &filter, None)? {
                entries.push(Some(envelope));
            }
        }

        if let Some(max) = max {
            while entries.len() < max {
                entries.push(None);
            }
        }

        Ok(entries)
    }
}

fn encode_packet(envelope: Envelope<TwoPcRound, Msg<u16>>) -> Vec<u8> {
    let (stamp, msg) = envelope.into_parts();
    let mut bytes = Vec::with_capacity(16);

    bytes.extend_from_slice(&stamp.instance.to_be_bytes());
    bytes.push(match stamp.phase {
        Phase::Vote => 0,
        Phase::Decision => 1,
    });

    match msg {
        Msg::Prepare { coordinator } => {
            bytes.push(0);
            bytes.extend_from_slice(&coordinator.to_be_bytes());
        }
        Msg::Vote(vote) => {
            bytes.push(1);
            bytes.push(u8::from(vote));
        }
        Msg::Decision(commit) => {
            bytes.push(2);
            bytes.push(u8::from(commit));
        }
    }

    bytes
}

fn decode_packet(bytes: &[u8]) -> io::Result<Envelope<TwoPcRound, Msg<u16>>> {
    if bytes.len() < 6 {
        return Err(invalid_packet("packet is too short"));
    }

    let instance = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
    let phase = match bytes[4] {
        0 => Phase::Vote,
        1 => Phase::Decision,
        _ => return Err(invalid_packet("unknown phase")),
    };
    let stamp = TwoPcRound { instance, phase };

    let msg = match bytes[5] {
        0 if bytes.len() == 8 => Msg::Prepare {
            coordinator: u16::from_be_bytes(bytes[6..8].try_into().unwrap()),
        },
        1 if bytes.len() == 7 => Msg::Vote(decode_bool(bytes[6])?),
        2 if bytes.len() == 7 => Msg::Decision(decode_bool(bytes[6])?),
        _ => return Err(invalid_packet("unknown message")),
    };

    Ok(Envelope::new(stamp, msg))
}

fn decode_bool(byte: u8) -> io::Result<bool> {
    match byte {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(invalid_packet("invalid boolean")),
    }
}

fn invalid_packet(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
