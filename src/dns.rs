//! The `msbx.test` resolver.
//!
//! `msbx dns` answers `A 127.0.0.1` for every name under `msbx.test` and
//! `NXDOMAIN` for everything else, as the zone contract requires. The wire
//! format is small and deliberately self-contained: one question, one A record,
//! no recursion. `respond` is pure so the protocol is tested without a socket.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The only zone `msbx dns` serves.
pub const ZONE: &str = "msbx.test";
/// The address the installed daemon listens on.
pub const LISTEN: &str = "127.0.0.1:5335";

const TYPE_A: u16 = 1;
const CLASS_IN: u16 = 1;
const RCODE_NXDOMAIN: u8 = 3;
const RCODE_NOTIMP: u8 = 4;
const TTL_SECS: u32 = 60;

/// A parsed DNS answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub rcode: u8,
    pub records: Vec<Ipv4Addr>,
}

impl Answer {
    pub fn nxdomain(&self) -> bool {
        self.rcode == RCODE_NXDOMAIN
    }
}

/// Build the response to one query packet, or `None` when the packet is not a
/// single well-formed standard query.
pub fn respond(packet: &[u8]) -> Option<Vec<u8>> {
    let (name, question_end, qtype, qclass) = parse_question(packet)?;
    let request_flags = be16(packet, 2)?;
    if request_flags & 0x8000 != 0 {
        // Already a response; never answer one.
        return None;
    }
    let opcode = (request_flags >> 11) & 0x0f;
    let recursion_desired = request_flags & 0x0100;

    let (rcode, answer) = if opcode != 0 {
        (RCODE_NOTIMP, false)
    } else if !in_zone(&name) {
        (RCODE_NXDOMAIN, false)
    } else if qtype == TYPE_A && qclass == CLASS_IN {
        (0, true)
    } else {
        // AAAA and friends are NODATA: the name exists, this type does not.
        (0, false)
    };

    let flags = 0x8000 | recursion_desired | 0x0080 | u16::from(rcode);
    let mut out = Vec::with_capacity(question_end + 16);
    out.extend_from_slice(&packet[0..2]);
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&u16::from(answer).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&packet[12..question_end]);

    if answer {
        // Compressed name pointer to the question at offset 12.
        out.extend_from_slice(&[0xc0, 0x0c]);
        out.extend_from_slice(&TYPE_A.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out.extend_from_slice(&TTL_SECS.to_be_bytes());
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(&Ipv4Addr::LOCALHOST.octets());
    }
    Some(out)
}

/// Ask `server` for `name`/`qtype` and parse the reply.
pub fn query(server: SocketAddr, name: &str, qtype: u16) -> io::Result<Answer> {
    let id = packet_id();
    let packet = question_packet(id, name, qtype)?;
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    socket.set_read_timeout(Some(Duration::from_secs(2)))?;
    socket.send_to(&packet, server)?;

    let mut buffer = [0u8; 1500];
    let (len, from) = socket.recv_from(&mut buffer)?;
    if from.ip() != server.ip() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("reply came from {from}, not {server}"),
        ));
    }
    parse_answer(&buffer[..len], id)
}

/// Is a working `msbx.test` resolver at `server`? It must answer the zone and
/// must not answer a name outside it.
pub fn answers(server: SocketAddr) -> bool {
    match query(server, "probe.msbx.test", TYPE_A) {
        Ok(answer) if answer.records == [Ipv4Addr::LOCALHOST] => {}
        _ => return false,
    }
    matches!(
        query(server, "probe.invalid", TYPE_A),
        Ok(answer) if answer.nxdomain()
    )
}

/// Bind and serve forever. Production entry point behind the installed unit.
pub fn serve(listen: SocketAddr) -> io::Result<()> {
    let socket = UdpSocket::bind(listen)?;
    println!("msbx dns listening on {listen} for {ZONE}");
    let mut buffer = [0u8; 1500];
    loop {
        match socket.recv_from(&mut buffer) {
            Ok((len, peer)) => {
                if let Some(response) = respond(&buffer[..len]) {
                    let _ = socket.send_to(&response, peer);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Serve one bound socket until `stop` is set. Split from [`serve`] so tests
/// can bind an ephemeral port and learn it.
pub fn serve_until(socket: &UdpSocket, stop: &AtomicBool) -> io::Result<()> {
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    let mut buffer = [0u8; 1500];
    while !stop.load(Ordering::Relaxed) {
        match socket.recv_from(&mut buffer) {
            Ok((len, peer)) => {
                if let Some(response) = respond(&buffer[..len]) {
                    let _ = socket.send_to(&response, peer);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn question_packet(id: u16, name: &str, qtype: u16) -> io::Result<Vec<u8>> {
    let mut packet = Vec::with_capacity(32);
    packet.extend_from_slice(&id.to_be_bytes());
    packet.extend_from_slice(&0x0100u16.to_be_bytes()); // recursion desired
    packet.extend_from_slice(&1u16.to_be_bytes());
    packet.extend_from_slice(&0u16.to_be_bytes());
    packet.extend_from_slice(&0u16.to_be_bytes());
    packet.extend_from_slice(&0u16.to_be_bytes());

    let trimmed = name.trim_end_matches('.');
    if trimmed.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty name"));
    }
    for label in trimmed.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name}: bad DNS label"),
            ));
        }
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0);
    packet.extend_from_slice(&qtype.to_be_bytes());
    packet.extend_from_slice(&CLASS_IN.to_be_bytes());
    Ok(packet)
}

fn parse_answer(packet: &[u8], id: u16) -> io::Result<Answer> {
    if packet.len() < 12 {
        return Err(invalid("reply is shorter than a DNS header"));
    }
    if be16(packet, 0) != Some(id) {
        return Err(invalid("reply id does not match the query"));
    }
    let flags = be16(packet, 2).ok_or_else(|| invalid("missing flags"))?;
    if flags & 0x8000 == 0 {
        return Err(invalid("packet is not a response"));
    }
    let rcode = (flags & 0x000f) as u8;
    let answers = be16(packet, 6).unwrap_or(0);

    let (_, mut offset, _, _) =
        parse_question(packet).ok_or_else(|| invalid("reply question is malformed"))?;
    let mut records = Vec::new();
    for _ in 0..answers {
        offset = skip_name(packet, offset)?;
        let rtype = be16(packet, offset).ok_or_else(|| invalid("answer is truncated"))?;
        let class = be16(packet, offset + 2).ok_or_else(|| invalid("answer is truncated"))?;
        let rdlen =
            be16(packet, offset + 8).ok_or_else(|| invalid("answer is truncated"))? as usize;
        let data = offset + 10;
        if data + rdlen > packet.len() {
            return Err(invalid("answer runs past the packet"));
        }
        if rtype == TYPE_A && class == CLASS_IN && rdlen == 4 {
            records.push(Ipv4Addr::new(
                packet[data],
                packet[data + 1],
                packet[data + 2],
                packet[data + 3],
            ));
        }
        offset = data + rdlen;
    }
    Ok(Answer { rcode, records })
}

/// Parse one question. Returns the name, the offset just past it, and the type
/// and class.
fn parse_question(packet: &[u8]) -> Option<(String, usize, u16, u16)> {
    if packet.len() < 12 || be16(packet, 4)? != 1 {
        return None;
    }
    let mut offset = 12;
    let mut labels: Vec<&str> = Vec::new();
    loop {
        let length = *packet.get(offset)?;
        offset += 1;
        if length == 0 {
            break;
        }
        if length & 0xc0 != 0 {
            // Compression is not expected in a query; refuse it rather than
            // risk misparsing a loop.
            return None;
        }
        let end = offset + usize::from(length);
        let label = std::str::from_utf8(packet.get(offset..end)?).ok()?;
        labels.push(label);
        offset = end;
        if labels.join(".").len() > 253 {
            return None;
        }
    }
    let qtype = be16(packet, offset)?;
    let qclass = be16(packet, offset + 2)?;
    Some((labels.join("."), offset + 4, qtype, qclass))
}

fn skip_name(packet: &[u8], start: usize) -> io::Result<usize> {
    let mut offset = start;
    loop {
        let length = *packet
            .get(offset)
            .ok_or_else(|| invalid("name runs past the packet"))?;
        if length == 0 {
            return Ok(offset + 1);
        }
        if length & 0xc0 == 0xc0 {
            return Ok(offset + 2);
        }
        offset += 1 + usize::from(length);
        if offset > packet.len() {
            return Err(invalid("name runs past the packet"));
        }
    }
}

fn in_zone(name: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    name == ZONE || name.ends_with(&format!(".{ZONE}"))
}

fn be16(packet: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes([
        *packet.get(offset)?,
        *packet.get(offset + 1)?,
    ]))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

fn packet_id() -> u16 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.subsec_nanos())
        .unwrap_or(0);
    (nanos as u16) ^ (std::process::id() as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query_bytes(name: &str, qtype: u16) -> Vec<u8> {
        question_packet(0x4242, name, qtype).unwrap()
    }

    #[test]
    fn answers_an_a_query_inside_the_zone() {
        let response = respond(&query_bytes("pg.mir42.signals.msbx.test", TYPE_A)).unwrap();
        let parsed = parse_answer(&response, 0x4242).unwrap();
        assert_eq!(parsed.rcode, 0);
        assert_eq!(parsed.records, vec![Ipv4Addr::LOCALHOST]);
        assert_eq!(be16(&response, 2).unwrap() & 0x8000, 0x8000, "QR set");
    }

    #[test]
    fn answers_the_zone_apex() {
        let response = respond(&query_bytes("msbx.test", TYPE_A)).unwrap();
        assert_eq!(
            parse_answer(&response, 0x4242).unwrap().records,
            vec![Ipv4Addr::LOCALHOST]
        );
    }

    #[test]
    fn nxdomain_outside_the_zone() {
        let response = respond(&query_bytes("hours.test", TYPE_A)).unwrap();
        let parsed = parse_answer(&response, 0x4242).unwrap();
        assert!(parsed.nxdomain());
        assert!(parsed.records.is_empty());
    }

    #[test]
    fn a_lookalike_suffix_is_outside_the_zone() {
        let response = respond(&query_bytes("notmsbx.test", TYPE_A)).unwrap();
        assert!(parse_answer(&response, 0x4242).unwrap().nxdomain());
    }

    #[test]
    fn aaaa_is_nodata_not_nxdomain() {
        let response = respond(&query_bytes("pg.msbx.test", 28)).unwrap();
        let parsed = parse_answer(&response, 0x4242).unwrap();
        assert_eq!(parsed.rcode, 0);
        assert!(parsed.records.is_empty());
    }

    #[test]
    fn malformed_queries_are_dropped() {
        assert!(respond(&[]).is_none());
        assert!(respond(&[0u8; 11]).is_none());
        let mut packet = query_bytes("pg.msbx.test", TYPE_A);
        packet[4] = 0; // qdcount 0
        packet[5] = 2;
        assert!(respond(&packet).is_none());
    }

    #[test]
    fn a_non_query_packet_is_dropped() {
        let mut packet = query_bytes("pg.msbx.test", TYPE_A);
        packet[2] |= 0x80; // QR set: this is a response, not a query
        assert!(respond(&packet).is_none());
    }

    #[test]
    fn serves_a_query_on_a_loopback_socket() {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = socket.local_addr().unwrap();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let handle = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let _ = serve_until(&socket, &stop);
            })
        };
        let answer = query(address, "web.msbx.test", TYPE_A).unwrap();
        assert_eq!(answer.records, vec![Ipv4Addr::LOCALHOST]);
        let outside = query(address, "example.com", TYPE_A).unwrap();
        assert!(outside.nxdomain());
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }

    #[test]
    fn a_bound_server_answers_the_probe() {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = socket.local_addr().unwrap();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let handle = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let _ = serve_until(&socket, &stop);
            })
        };
        assert!(answers(address));
        stop.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }
}
