use anyhow::{Context, Result, bail};
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};

const MAX_PACKET_SIZE: usize = 1024 * 1024;

pub fn handshake(stream: &mut TcpStream) -> Result<()> {
    // DeskDock is a Synergy client, so this local bridge sends the greeting.
    let name = b"hypr-android-bridge";
    let mut greeting = Vec::with_capacity(11 + 4 + name.len());
    greeting.extend_from_slice(b"Barrier");
    greeting.extend_from_slice(&1_u16.to_be_bytes());
    greeting.extend_from_slice(&6_u16.to_be_bytes());
    greeting.extend_from_slice(&(name.len() as u32).to_be_bytes());
    greeting.extend_from_slice(name);
    send_frame(stream, &greeting)?;

    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let hello = read_frame(stream).context("DeskDock did not reply to the Synergy handshake")?;
    if hello.len() < 11 || &hello[..7] != b"Barrier" {
        bail!("Unexpected Synergy handshake from DeskDock: {hello:?}");
    }
    let version = (
        u16::from_be_bytes([hello[7], hello[8]]),
        u16::from_be_bytes([hello[9], hello[10]]),
    );
    eprintln!(
        "DeskDock accepted Synergy/Barrier protocol {}.{}.",
        version.0, version.1
    );
    stream.set_read_timeout(None)?;
    Ok(())
}

pub fn drain_client_messages(stream: &mut TcpStream, width: i16, height: i16) -> Result<()> {
    let mut length = [0; 4];
    match stream.read_exact(&mut length) {
        Ok(()) => {
            let length = u32::from_be_bytes(length) as usize;
            if length > MAX_PACKET_SIZE {
                bail!("DeskDock sent an implausibly large Synergy packet");
            }
            let mut packet = vec![0; length];
            stream.read_exact(&mut packet)?;
            if packet.as_slice() == b"QINF" {
                send_frame(stream, &screen_info(width, height))?;
            } else if packet.as_slice() == b"CALV" {
                send_frame(stream, b"CALV")?;
            }
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::WouldBlock
                || error.kind() == std::io::ErrorKind::TimedOut => {}
        Err(error) => return Err(error).context("DeskDock Synergy connection was lost"),
    }
    Ok(())
}

pub fn send_frame(stream: &mut TcpStream, payload: &[u8]) -> Result<()> {
    let length = u32::try_from(payload.len()).context("Synergy packet is too large")?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(payload)?;
    stream.flush()?;
    Ok(())
}

fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut length = [0; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_PACKET_SIZE {
        bail!("Synergy packet is implausibly large ({length} bytes)");
    }
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload)?;
    Ok(payload)
}

pub fn server_options() -> Vec<u8> {
    let mut packet = b"DSOP".to_vec();
    packet.extend_from_slice(&0_u32.to_be_bytes());
    packet
}

pub fn screen_info(width: i16, height: i16) -> Vec<u8> {
    let mut packet = b"DINF".to_vec();
    for value in [0_i16, 0, width, height, 0, width / 2, height / 2] {
        packet.extend_from_slice(&value.to_be_bytes());
    }
    packet
}

pub fn client_enter(width: i16, height: i16) -> Vec<u8> {
    let mut packet = b"CINN".to_vec();
    packet.extend_from_slice(&width.to_be_bytes());
    packet.extend_from_slice(&height.to_be_bytes());
    packet.extend_from_slice(&0_u16.to_be_bytes());
    packet.extend_from_slice(&0_u32.to_be_bytes());
    packet
}

pub fn mouse_move(x: i16, y: i16) -> Vec<u8> {
    let mut packet = b"DMMV".to_vec();
    packet.extend_from_slice(&x.to_be_bytes());
    packet.extend_from_slice(&y.to_be_bytes());
    packet
}

pub fn mouse_button(down: bool, button: u8) -> Vec<u8> {
    let mut packet = if down {
        b"DMDN".to_vec()
    } else {
        b"DMUP".to_vec()
    };
    packet.push(button);
    packet
}

pub fn mouse_wheel(x: i16, y: i16) -> Vec<u8> {
    let mut packet = b"DMWM".to_vec();
    packet.extend_from_slice(&x.to_be_bytes());
    packet.extend_from_slice(&y.to_be_bytes());
    packet
}
