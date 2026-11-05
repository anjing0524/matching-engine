//! Event-driven front gateway.
//!
//! Fixed-thread count: each `mio` event loop manages a listener, an upstream
//! Unix socket to the matching process, and its client connections. The loop
//! count is `--io-threads` and does not grow with connections. Slow clients are
//! bounded by per-connection write buffers and in-flight caps; the matching
//! process is decoupled through IPC.
use clap::Parser;
use matching_engine::interfaces::engine_ipc::{
    IpcRequest, MAX_IPC_PAYLOAD, decode_response, encode_request, ring_paths,
};
use matching_engine::shared::spsc_ring::{PushError, SpscRing};
use mio::net::{TcpListener, TcpStream, UnixStream};
use mio::{Events, Interest, Poll, Token};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const LISTENER: Token = Token(0);
const UPSTREAM: Token = Token(1);
const FIRST_CLIENT: u64 = 2;

#[derive(Parser, Debug, Clone)]
#[command(name = "gateway")]
#[command(about = "Event-driven front gateway for the matching engine")]
struct Config {
    /// Client TCP listen address.
    #[arg(long, default_value = "127.0.0.1:8080")]
    listen: String,

    /// Engine IPC Unix socket path.
    #[arg(long, default_value = "./data/engine.sock")]
    engine_uds: PathBuf,

    /// Maximum simultaneous client connections.
    #[arg(long, default_value_t = 100_000)]
    max_connections: usize,

    /// Maximum in-flight requests per client connection.
    #[arg(long, default_value_t = 64)]
    max_in_flight: usize,

    /// Maximum buffered bytes per client (read + write).
    #[arg(long, default_value_t = 1024 * 1024)]
    max_client_buffer: usize,

    /// Maximum queued upstream bytes before gateway exits.
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    max_upstream_buffer: usize,

    /// Poll timeout in milliseconds.
    #[arg(long, default_value_t = 10)]
    poll_timeout_ms: u64,

    /// Number of fixeds event-loop threads. Each loop owns one engine IPC socket.
    #[arg(long, default_value_t = 1)]
    io_threads: usize,

    /// Use the engine shared-memory SPSC rings in this directory instead of UDS.
    #[arg(long)]
    ipc_ring_dir: Option<PathBuf>,
}

struct Client {
    stream: TcpStream,
    id: u64,
    read_buf: Vec<u8>,
    write_buf: Vec<u8>,
    in_flight: usize,
    read_paused: bool,
    registered: bool,
}

struct Upstream {
    stream: UnixStream,
    read_buf: Vec<u8>,
    write_buf: Vec<u8>,
    write_registered: bool,
}

fn connect_engine(path: &std::path::Path) -> io::Result<UnixStream> {
    let mut last_error = None;
    for _ in 0..100 {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                last_error = Some(error);
                thread::sleep(Duration::from_millis(100));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "engine IPC socket not available")
    }))
}

fn main() {
    if let Err(error) = run() {
        eprintln!("gateway error: {error}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let config = Config::parse();
    if config.io_threads == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--io-threads must be greater than zero",
        ));
    }
    let addr: SocketAddr = config
        .listen
        .parse()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if addr.port() == 0 && config.io_threads > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "port 0 cannot be combined with multiple io threads",
        ));
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    let signal_flag = Arc::clone(&shutdown);
    let mut signals = Signals::new([SIGINT, SIGTERM])?;
    thread::Builder::new()
        .name("gateway-signals".to_string())
        .spawn(move || {
            if signals.forever().next().is_some() {
                signal_flag.store(true, Ordering::Release);
            }
        })?;

    let config = Arc::new(config);
    let connections = Arc::new(AtomicUsize::new(0));
    if let Some(ring_dir) = config.ipc_ring_dir.clone() {
        let mut handles = Vec::with_capacity(config.io_threads);
        for loop_id in 0..config.io_threads {
            let config = Arc::clone(&config);
            let shutdown = Arc::clone(&shutdown);
            let connections = Arc::clone(&connections);
            let listener = bind_listener(addr, config.io_threads > 1)?;
            let local = listener.local_addr()?;
            println!(
                "gateway ring loop {loop_id} listening on {local}, rings {}",
                ring_dir.display()
            );
            let ring_dir = ring_dir.clone();
            handles.push(
                thread::Builder::new()
                    .name(format!("gateway-ring-{loop_id}"))
                    .spawn(move || {
                        run_ring_loop(config, listener, shutdown, connections, ring_dir, loop_id)
                    })?,
            );
        }
        return join_loops(handles, shutdown);
    }
    let mut handles = Vec::with_capacity(config.io_threads);
    for loop_id in 0..config.io_threads {
        let config = Arc::clone(&config);
        let shutdown = Arc::clone(&shutdown);
        let connections = Arc::clone(&connections);
        let listener = bind_listener(addr, config.io_threads > 1)?;
        let local = listener.local_addr()?;
        println!(
            "gateway loop {loop_id} listening on {local}, engine {}",
            config.engine_uds.display()
        );
        handles.push(
            thread::Builder::new()
                .name(format!("gateway-io-{loop_id}"))
                .spawn(move || run_loop(config, listener, shutdown, connections))?,
        );
    }

    let mut first_error: Option<io::Error> = None;
    for handle in handles {
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                first_error.get_or_insert(error);
                shutdown.store(true, Ordering::Release);
            }
            Err(_) => {
                first_error.get_or_insert_with(error_from_join);
                shutdown.store(true, Ordering::Release);
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(())
}

fn join_loops(
    mut handles: Vec<JoinHandle<io::Result<()>>>,
    shutdown: Arc<AtomicBool>,
) -> io::Result<()> {
    let mut first_error = None;
    while let Some(handle) = handles.pop() {
        match handle.join() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                first_error.get_or_insert(error);
                shutdown.store(true, Ordering::Release);
            }
            Err(_) => {
                first_error.get_or_insert_with(error_from_join);
                shutdown.store(true, Ordering::Release);
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(())
}

fn error_from_join() -> io::Error {
    io::Error::other("gateway event loop terminated")
}

fn bind_listener(addr: SocketAddr, reuse_port: bool) -> io::Result<TcpListener> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    if reuse_port {
        socket.set_reuse_port(true)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    let listener: std::net::TcpListener = socket.into();
    Ok(TcpListener::from_std(listener))
}

fn run_loop(
    config: Arc<Config>,
    mut listener: TcpListener,
    shutdown: Arc<AtomicBool>,
    connections: Arc<AtomicUsize>,
) -> io::Result<()> {
    let upstream_stream = connect_engine(&config.engine_uds)?;
    let mut upstream = Upstream {
        stream: upstream_stream,
        read_buf: Vec::with_capacity(64 * 1024),
        write_buf: Vec::with_capacity(64 * 1024),
        write_registered: false,
    };

    let mut poll = Poll::new()?;
    poll.registry()
        .register(&mut listener, LISTENER, Interest::READABLE)?;
    poll.registry()
        .register(&mut upstream.stream, UPSTREAM, Interest::READABLE)?;

    let mut events = Events::with_capacity(4096);
    let mut clients: HashMap<Token, Client> = HashMap::new();
    let mut next_token = FIRST_CLIENT;
    let timeout = Duration::from_millis(config.poll_timeout_ms.max(1));

    while !shutdown.load(Ordering::Acquire) {
        poll.poll(&mut events, Some(timeout))?;
        for event in events.iter() {
            match event.token() {
                LISTENER => accept_clients(
                    &mut listener,
                    &mut poll,
                    &mut clients,
                    &mut next_token,
                    &config,
                    &connections,
                )?,
                UPSTREAM => {
                    if !service_upstream(&mut upstream, &mut poll, &mut clients, &config)? {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "engine IPC closed",
                        ));
                    }
                }
                token => {
                    if !service_client(token, &mut poll, &mut clients, &mut upstream, &config)? {
                        clients.remove(&token);
                        connections.fetch_sub(1, Ordering::AcqRel);
                    }
                }
            }
        }
    }
    Ok(())
}

struct RingTransport {
    request: SpscRing,
    response: SpscRing,
    response_buf: Vec<u8>,
}

fn connect_rings(dir: &std::path::Path, index: usize) -> io::Result<RingTransport> {
    let (request_path, response_path) = ring_paths(dir, index);
    let mut last = None;
    for _ in 0..300 {
        if request_path.exists() && response_path.exists() {
            match (
                SpscRing::open(&request_path),
                SpscRing::open(&response_path),
            ) {
                (Ok(request), Ok(response)) => {
                    return Ok(RingTransport {
                        request,
                        response,
                        response_buf: Vec::with_capacity(16 * 1024),
                    });
                }
                (request, response) => {
                    last = request.err().or_else(|| response.err());
                }
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err(last.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "engine SPSC rings not available")
    }))
}

fn run_ring_loop(
    config: Arc<Config>,
    mut listener: TcpListener,
    shutdown: Arc<AtomicBool>,
    connections: Arc<AtomicUsize>,
    ring_dir: PathBuf,
    ring_index: usize,
) -> io::Result<()> {
    let mut ring = connect_rings(&ring_dir, ring_index)?;
    let mut poll = Poll::new()?;
    poll.registry()
        .register(&mut listener, LISTENER, Interest::READABLE)?;
    let mut events = Events::with_capacity(4096);
    let mut clients: HashMap<Token, Client> = HashMap::new();
    let mut next_token = FIRST_CLIENT;
    while !shutdown.load(Ordering::Acquire) {
        // Poll timeout zero keeps the response ring latency bounded by the
        // event-loop iteration rather than a timer.
        poll.poll(&mut events, Some(Duration::ZERO))?;
        for event in events.iter() {
            match event.token() {
                LISTENER => accept_clients(
                    &mut listener,
                    &mut poll,
                    &mut clients,
                    &mut next_token,
                    &config,
                    &connections,
                )?,
                token => {
                    if !service_client_ring(
                        token,
                        &mut poll,
                        &mut clients,
                        &ring,
                        &config,
                        &shutdown,
                    )? {
                        clients.remove(&token);
                        connections.fetch_sub(1, Ordering::AcqRel);
                    }
                }
            }
        }
        drain_ring_responses(&mut ring, &mut poll, &mut clients, &config, &shutdown)?;
        std::hint::spin_loop();
    }
    Ok(())
}

fn service_client_ring(
    token: Token,
    poll: &mut Poll,
    clients: &mut HashMap<Token, Client>,
    ring: &RingTransport,
    config: &Config,
    shutdown: &AtomicBool,
) -> io::Result<bool> {
    let Some(client) = clients.get_mut(&token) else {
        return Ok(false);
    };

    if !client.write_buf.is_empty() {
        while !client.write_buf.is_empty() {
            match client.stream.write(&client.write_buf) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    client.write_buf.drain(..n);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Ok(false),
            }
        }
    }

    if !client.read_paused {
        loop {
            let mut buf = [0u8; 64 * 1024];
            match client.stream.read(&mut buf) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    client.read_buf.extend_from_slice(&buf[..n]);
                    if client.read_buf.len() > config.max_client_buffer {
                        return Ok(false);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Ok(false),
            }
        }
        while client.read_buf.len() >= 4 {
            let len =
                u32::from_be_bytes(client.read_buf[..4].try_into().expect("4 bytes")) as usize;
            if len == 0 || len > MAX_IPC_PAYLOAD {
                return Ok(false);
            }
            if client.read_buf.len() < 4 + len {
                break;
            }
            if client.in_flight >= config.max_in_flight {
                client.read_paused = true;
                poll.registry().deregister(&mut client.stream)?;
                client.registered = false;
                break;
            }
            let payload = client.read_buf[4..4 + len].to_vec();
            client.read_buf.drain(..4 + len);
            let bytes = encode_request(&IpcRequest {
                client_id: client.id,
                payload,
            });
            let deadline = Instant::now() + Duration::from_millis(100);
            loop {
                match ring.request.try_push(&bytes) {
                    Ok(()) => break,
                    Err(PushError::Full) if Instant::now() < deadline => {
                        std::hint::spin_loop();
                    }
                    Err(PushError::Full) => {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "engine ring is not draining",
                        ));
                    }
                    Err(PushError::TooLarge) => return Ok(false),
                }
                if shutdown.load(Ordering::Acquire) {
                    return Ok(false);
                }
            }
            client.in_flight += 1;
        }
    }

    if !client.registered && !client.read_paused {
        let interest = client_interest(client);
        poll.registry()
            .register(&mut client.stream, token, interest)?;
        client.registered = true;
    }
    if client.registered {
        let interest = client_interest(client);
        poll.registry()
            .reregister(&mut client.stream, token, interest)?;
    }
    Ok(true)
}

fn drain_ring_responses(
    ring: &mut RingTransport,
    poll: &mut Poll,
    clients: &mut HashMap<Token, Client>,
    config: &Config,
    shutdown: &AtomicBool,
) -> io::Result<()> {
    let mut unpaused: Vec<Token> = Vec::new();
    loop {
        if !ring.response.try_pop_into(&mut ring.response_buf) {
            break;
        }
        let Some((response, _)) = decode_response(&ring.response_buf) else {
            continue;
        };
        let token = Token(response.client_id as usize);
        let Some(client) = clients.get_mut(&token) else {
            continue;
        };
        for payload in response.payloads {
            let needed = client.write_buf.len() + payload.len() + 4;
            if needed > config.max_client_buffer {
                return Ok(());
            }
            client
                .write_buf
                .extend_from_slice(&(payload.len() as u32).to_be_bytes());
            client.write_buf.extend_from_slice(&payload);
        }
        if client.in_flight > 0 {
            client.in_flight -= 1;
        }
        if client.read_paused && client.in_flight < config.max_in_flight {
            client.read_paused = false;
            if !client.registered {
                let interest = client_interest(client);
                poll.registry()
                    .register(&mut client.stream, token, interest)?;
                client.registered = true;
            }
            unpaused.push(token);
        }
        let interest = client_interest(client);
        poll.registry()
            .reregister(&mut client.stream, token, interest)?;
    }
    for token in unpaused {
        let _ = service_client_ring(token, poll, clients, ring, config, shutdown)?;
    }
    Ok(())
}

fn accept_clients(
    listener: &mut TcpListener,
    poll: &mut Poll,
    clients: &mut HashMap<Token, Client>,
    next_token: &mut u64,
    config: &Config,
    connections: &AtomicUsize,
) -> io::Result<()> {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if connections.fetch_add(1, Ordering::AcqRel) >= config.max_connections {
                    connections.fetch_sub(1, Ordering::AcqRel);
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    continue;
                }
                stream.set_nodelay(true)?;
                let token = Token(*next_token as usize);
                *next_token += 1;
                let mut stream = stream;
                poll.registry()
                    .register(&mut stream, token, Interest::READABLE)?;
                clients.insert(
                    token,
                    Client {
                        stream,
                        id: token.0 as u64,
                        read_buf: Vec::with_capacity(16 * 1024),
                        write_buf: Vec::with_capacity(16 * 1024),
                        in_flight: 0,
                        read_paused: false,
                        registered: true,
                    },
                );
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn service_client(
    token: Token,
    poll: &mut Poll,
    clients: &mut HashMap<Token, Client>,
    upstream: &mut Upstream,
    config: &Config,
) -> io::Result<bool> {
    let Some(client) = clients.get_mut(&token) else {
        return Ok(false);
    };

    if upstream.write_buf.len() > config.max_upstream_buffer {
        return Err(io::Error::new(
            io::ErrorKind::OutOfMemory,
            "upstream write buffer limit exceeded",
        ));
    }

    if !client.write_buf.is_empty() {
        while !client.write_buf.is_empty() {
            match client.stream.write(&client.write_buf) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    client.write_buf.drain(..n);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Ok(false),
            }
        }
    }

    if !client.read_paused {
        loop {
            let mut buf = [0u8; 64 * 1024];
            match client.stream.read(&mut buf) {
                Ok(0) => return Ok(false),
                Ok(n) => {
                    client.read_buf.extend_from_slice(&buf[..n]);
                    if client.read_buf.len() > config.max_client_buffer {
                        return Ok(false);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Ok(false),
            }
        }

        while client.read_buf.len() >= 4 {
            let len =
                u32::from_be_bytes(client.read_buf[..4].try_into().expect("4 bytes")) as usize;
            if len == 0 || len > MAX_IPC_PAYLOAD {
                return Ok(false);
            }
            if client.read_buf.len() < 4 + len {
                break;
            }
            if client.in_flight >= config.max_in_flight {
                client.read_paused = true;
                poll.registry().deregister(&mut client.stream)?;
                client.registered = false;
                break;
            }
            let payload = client.read_buf[4..4 + len].to_vec();
            client.read_buf.drain(..4 + len);
            upstream
                .write_buf
                .extend_from_slice(&encode_request(&IpcRequest {
                    client_id: client.id,
                    payload,
                }));
            client.in_flight += 1;
        }
    }

    if !client.registered && !client.read_paused {
        let interest = client_interest(client);
        poll.registry()
            .register(&mut client.stream, token, interest)?;
        client.registered = true;
    }
    if client.registered {
        let interest = client_interest(client);
        poll.registry()
            .reregister(&mut client.stream, token, interest)?;
    }
    if upstream.write_buf.len() > config.max_upstream_buffer {
        return Err(io::Error::new(
            io::ErrorKind::OutOfMemory,
            "upstream write buffer limit exceeded",
        ));
    }
    let _ = service_upstream_writable(upstream, poll);
    Ok(true)
}

fn client_interest(client: &Client) -> Interest {
    let mut interest = Interest::READABLE;
    if !client.write_buf.is_empty() {
        interest |= Interest::WRITABLE;
    }
    interest
}

fn service_upstream(
    upstream: &mut Upstream,
    poll: &mut Poll,
    clients: &mut HashMap<Token, Client>,
    config: &Config,
) -> io::Result<bool> {
    service_upstream_writable(upstream, poll)?;
    loop {
        let mut buf = [0u8; 64 * 1024];
        match upstream.stream.read(&mut buf) {
            Ok(0) => return Ok(false),
            Ok(n) => upstream.read_buf.extend_from_slice(&buf[..n]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
        if upstream.read_buf.len() > config.max_upstream_buffer {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "upstream read buffer limit exceeded",
            ));
        }
    }
    let mut unpaused = Vec::new();
    while let Some((response, consumed)) = decode_response(&upstream.read_buf) {
        upstream.read_buf.drain(..consumed);
        let token = Token(response.client_id as usize);
        if let Some(client) = clients.get_mut(&token) {
            for payload in response.payloads {
                if client.write_buf.len() + payload.len() + 4 > config.max_client_buffer {
                    return Ok(false);
                }
                client
                    .write_buf
                    .extend_from_slice(&(payload.len() as u32).to_be_bytes());
                client.write_buf.extend_from_slice(&payload);
            }
            if client.in_flight > 0 {
                client.in_flight -= 1;
            }
            if client.read_paused && client.in_flight < config.max_in_flight {
                client.read_paused = false;
                if !client.registered {
                    let interest = client_interest(client);
                    poll.registry()
                        .register(&mut client.stream, token, interest)?;
                    client.registered = true;
                }
                unpaused.push(token);
            }
            let interest = client_interest(client);
            poll.registry()
                .reregister(&mut client.stream, token, interest)?;
        }
    }
    for token in unpaused {
        let _ = service_client(token, poll, clients, upstream, config)?;
    }
    Ok(true)
}

fn service_upstream_writable(upstream: &mut Upstream, poll: &mut Poll) -> io::Result<()> {
    if upstream.write_buf.is_empty() {
        if upstream.write_registered {
            poll.registry()
                .reregister(&mut upstream.stream, UPSTREAM, Interest::READABLE)?;
            upstream.write_registered = false;
        }
        return Ok(());
    }
    while !upstream.write_buf.is_empty() {
        match upstream.stream.write(&upstream.write_buf) {
            Ok(0) => break,
            Ok(n) => {
                upstream.write_buf.drain(..n);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    let interest = if upstream.write_buf.is_empty() {
        upstream.write_registered = false;
        Interest::READABLE
    } else {
        upstream.write_registered = true;
        Interest::READABLE | Interest::WRITABLE
    };
    poll.registry()
        .reregister(&mut upstream.stream, UPSTREAM, interest)?;
    Ok(())
}
