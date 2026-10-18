//! gdb-remote pieces of StikDebug's universal JIT protocol (IOS-JIT-1). The
//! player raises `brk #0xf00d` with the command in x16: 1 prepares a
//! read-execute region of x1 bytes (x0 = null asks the debugger to allocate)
//! and returns it in x0; 0 detaches.

use idevice::{ReadWrite, debug_proxy::DebugProxyClient};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub type Debugger = DebugProxyClient<Box<dyn ReadWrite>>;

pub async fn attach(
    debugger: &mut Debugger,
    pid: u32,
    script: bool,
    progress: &impl Fn(&str),
) -> Result<()> {
    debugger.send_ack().await?;
    debugger.send_ack().await?;
    expect_ok(debugger, "QStartNoAckMode").await?;
    debugger.set_ack_mode(false);
    expect_ok(debugger, "QSetDetachOnError:1").await?;
    expect_ok(debugger, "QThreadSuffixSupported").await?;
    progress("attaching");
    let stop = command(debugger, &format!("vAttach;{pid:x}")).await?;
    let result: Result<()> = async {
        stopped_thread(&stop)?;
        if script {
            progress("preparing");
            serve(debugger).await?;
        }
        Ok(())
    }
    .await;
    // A failed JIT request must not leave the player suspended.
    let detach = expect_ok(debugger, "D").await;
    result?;
    detach
}

/// `brk #0xf00d`.
pub const BRK_F00D: u32 = 0xD43E_01A0;
/// TXM accepts a page as debugger-prepared once the debugger wrote to it.
pub const PAGE: u64 = 16 << 10;
pub const X0: u32 = 0;
pub const X1: u32 = 1;
pub const X16: u32 = 0x10;
pub const PC: u32 = 0x20;

/// A complete packet: `$body#checksum`.
pub fn packet(body: &str) -> String {
    let sum = body.bytes().fold(0u8, |sum, byte| sum.wrapping_add(byte));
    format!("${body}#{sum:02x}")
}

/// The packets that write one byte into each page of a region, in order.
pub fn page_writes(address: u64, len: u64) -> impl Iterator<Item = String> {
    (0..len.div_ceil(PAGE)).map(move |page| packet(&format!("M{:x},1:00", address + page * PAGE)))
}

/// The thread a stop reply (`T05thread:1c03;…`) names, or why it names none.
pub fn stopped_thread(reply: &str) -> std::result::Result<&str, String> {
    if reply.starts_with('W') || reply.starts_with('X') {
        return Err(format!("the player exited ({reply})"));
    }
    reply
        .get(3..)
        .filter(|_| reply.starts_with('T'))
        .and_then(|fields| {
            fields
                .split(';')
                .find_map(|field| field.strip_prefix("thread:"))
        })
        .ok_or_else(|| format!("unexpected stop reply {reply:?}"))
}

/// Little-endian hex bytes, as register and memory reads return them.
pub fn le_value(hex: &str) -> Option<u64> {
    if hex.is_empty() || hex.len() > 16 || !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut value = 0u64;
    for (index, pair) in hex.as_bytes().chunks(2).enumerate() {
        let byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
        value |= u64::from(byte) << (8 * index);
    }
    Some(value)
}

/// The register-write packet body for a 64-bit register.
pub fn write_register(tid: &str, number: u32, value: u64) -> String {
    let bytes: String = value
        .to_le_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("P{number:x}={bytes};thread:{tid};")
}

async fn command(debugger: &mut Debugger, body: &str) -> Result<String> {
    debugger.send_raw(packet(body).as_bytes()).await?;
    debugger
        .read_response()
        .await?
        .ok_or_else(|| "missing debugger response".into())
}

async fn expect_ok(debugger: &mut Debugger, body: &str) -> Result<()> {
    let reply = command(debugger, body).await?;
    if reply != "OK" {
        return Err(format!("{body}: {reply}").into());
    }
    Ok(())
}

async fn register(debugger: &mut Debugger, tid: &str, number: u32) -> Result<u64> {
    let reply = command(debugger, &format!("p{number:x};thread:{tid};")).await?;
    le_value(&reply).ok_or_else(|| "invalid debugger register value".into())
}

async fn serve(debugger: &mut Debugger) -> Result<()> {
    let info = command(debugger, "qHostInfo").await?;
    let bits: u32 = info
        .split(';')
        .find_map(|f| f.strip_prefix("addressing_bits:"))
        .unwrap_or("47")
        .parse()?;
    let mask = u64::MAX
        .checked_shr(64 - bits)
        .ok_or("invalid address width")?;
    loop {
        let stop = command(debugger, "c").await?;
        let tid = stopped_thread(&stop)?;
        let pc = register(debugger, tid, PC).await?;
        let word = command(debugger, &format!("m{:x},4", pc & mask)).await?;
        if le_value(&word) != Some(u64::from(BRK_F00D)) {
            return Err("player stopped outside a JIT request".into());
        }
        let operation = register(debugger, tid, X16).await?;
        if operation > 1 {
            return Err(format!("unknown JIT request {operation}").into());
        }
        expect_ok(debugger, &write_register(tid, PC, pc + 4)).await?;
        if operation == 0 {
            return Ok(());
        }
        let mut address = register(debugger, tid, X0).await?;
        let size = register(debugger, tid, X1).await?;
        if address == 0 {
            let reply = command(debugger, &format!("_M{size:x},rx")).await?;
            if reply.starts_with('E') {
                return Err(format!("JIT allocation failed: {reply}").into());
            }
            address = u64::from_str_radix(&reply, 16)?;
        }
        for packet in page_writes(address, size) {
            debugger.send_raw(packet.as_bytes()).await?;
            if debugger.read_response().await?.as_deref() != Some("OK") {
                return Err("debugger could not prepare a code page".into());
            }
        }
        expect_ok(debugger, &write_register(tid, X0, address)).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: IOS-JIT-1
    #[test]
    fn packets_carry_their_checksum() {
        assert_eq!(packet("c"), "$c#63");
        assert_eq!(packet("QStartNoAckMode"), "$QStartNoAckMode#b0");
    }

    // spec: IOS-JIT-1
    #[test]
    fn every_started_page_is_written() {
        let writes: Vec<_> = page_writes(0x1_0000_0000, PAGE + 1).collect();
        assert_eq!(writes.len(), 2);
        assert!(writes[0].starts_with("$M100000000,1:00#"));
        assert!(writes[1].starts_with("$M100004000,1:00#"));
    }

    // spec: IOS-JIT-1
    #[test]
    fn stop_replies_name_their_thread() {
        assert_eq!(stopped_thread("T05thread:1c03;name:main;"), Ok("1c03"));
        assert!(stopped_thread("W00").is_err());
        assert!(stopped_thread("OK").is_err());
    }

    // spec: IOS-JIT-1
    #[test]
    fn values_are_little_endian() {
        assert_eq!(le_value("a0013ed4"), Some(BRK_F00D as u64));
        assert_eq!(le_value("0040000001000000"), Some(0x1_0000_4000));
        assert_eq!(le_value("xyz"), None);
        assert_eq!((X0, X1, X16), (0, 1, 0x10));
        assert_eq!(
            write_register("1c", PC, 0x1_0000_4000),
            "P20=0040000001000000;thread:1c;"
        );
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn session(script: bool, exchanges: Vec<(&str, &str)>) -> Result<()> {
        let (client, mut server) = tokio::io::duplex(4096);
        let exchanges: Vec<_> = exchanges
            .into_iter()
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
            .collect();
        let peer = tokio::spawn(async move {
            let mut hello = [0; 2];
            server.read_exact(&mut hello).await.unwrap();
            assert_eq!(&hello, b"++");
            for (body, response) in exchanges {
                let expected = packet(&body);
                let mut received = vec![0; expected.len()];
                server.read_exact(&mut received).await.unwrap();
                assert_eq!(received, expected.as_bytes());
                if body == "QStartNoAckMode" {
                    server.write_all(b"+").await.unwrap();
                }
                server
                    .write_all(packet(&response).as_bytes())
                    .await
                    .unwrap();
                if body == "QStartNoAckMode" {
                    assert_eq!(server.read_u8().await.unwrap(), b'+');
                }
            }
        });
        let mut debugger = Debugger::new(Box::new(client));
        let result = attach(&mut debugger, 42, script, &|_| {}).await;
        peer.await.unwrap();
        result
    }

    fn handshake() -> Vec<(&'static str, &'static str)> {
        vec![
            ("QStartNoAckMode", "OK"),
            ("QSetDetachOnError:1", "OK"),
            ("QThreadSuffixSupported", "OK"),
            ("vAttach;2a", "T05thread:1;"),
        ]
    }

    // spec: IOS-JIT-2
    #[tokio::test]
    async fn older_ios_attaches_then_detaches_without_allocating() {
        let mut exchanges = handshake();
        exchanges.push(("D", "OK"));
        session(false, exchanges).await.unwrap();
    }

    // spec: IOS-JIT-1
    #[tokio::test]
    async fn universal_call_allocates_touches_each_page_and_detaches() {
        let mut exchanges = handshake();
        exchanges.extend([
            ("qHostInfo", "addressing_bits:47;"),
            ("c", "T05thread:1;"),
            ("p20;thread:1;", "0010000000000000"),
            ("m1000,4", "a0013ed4"),
            ("p10;thread:1;", "0100000000000000"),
            ("P20=0410000000000000;thread:1;", "OK"),
            ("p0;thread:1;", "0000000000000000"),
            ("p1;thread:1;", "0140000000000000"),
            ("_M4001,rx", "100000000"),
            ("M100000000,1:00", "OK"),
            ("M100004000,1:00", "OK"),
            ("P0=0000000001000000;thread:1;", "OK"),
            ("c", "T05thread:1;"),
            ("p20;thread:1;", "0020000000000000"),
            ("m2000,4", "a0013ed4"),
            ("p10;thread:1;", "0000000000000000"),
            ("P20=0420000000000000;thread:1;", "OK"),
            ("D", "OK"),
        ]);
        session(true, exchanges).await.unwrap();
    }

    // spec: IOS-JIT-2
    #[tokio::test]
    async fn allocation_failure_detaches_and_reports_failure() {
        let mut exchanges = handshake();
        exchanges.extend([
            ("qHostInfo", "addressing_bits:47;"),
            ("c", "T05thread:1;"),
            ("p20;thread:1;", "0010000000000000"),
            ("m1000,4", "a0013ed4"),
            ("p10;thread:1;", "0100000000000000"),
            ("P20=0410000000000000;thread:1;", "OK"),
            ("p0;thread:1;", "0000000000000000"),
            ("p1;thread:1;", "0040000000000000"),
            ("_M4000,rx", "E09"),
            ("D", "OK"),
        ]);
        assert!(
            session(true, exchanges)
                .await
                .unwrap_err()
                .to_string()
                .contains("allocation failed")
        );
    }

    // spec: IOS-JIT-2
    #[tokio::test]
    async fn unrelated_stop_detaches_without_changing_registers() {
        let mut exchanges = handshake();
        exchanges.extend([
            ("qHostInfo", "addressing_bits:47;"),
            ("c", "T05thread:1;"),
            ("p20;thread:1;", "0010000000000000"),
            ("m1000,4", "00000000"),
            ("D", "OK"),
        ]);
        assert!(
            session(true, exchanges)
                .await
                .unwrap_err()
                .to_string()
                .contains("outside a JIT request")
        );
    }
}
