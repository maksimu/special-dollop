#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveProtocol {
    Socks5,
    Guacd,
    PortForward,
}
