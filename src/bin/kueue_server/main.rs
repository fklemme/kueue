mod client_connection;
mod job_manager;
mod worker_connection;

use client_connection::ClientConnection;
use job_manager::Manager;
use kueue::{
    config::Config,
    messages::stream::MessageStream,
    messages::{HelloMessage, ServerToClientMessage, ServerToWorkerMessage},
};
use simple_logger::SimpleLogger;
use std::{
    error::Error,
    sync::{Arc, Mutex},
};
use tokio::{
    net::{TcpListener, TcpStream},
    time::{sleep, Duration},
    try_join,
};
use worker_connection::WorkerConnection;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Read configuration from file or defaults.
    let config = Config::new()?;
    // If there is no config file, create template.
    if let Err(e) = config.create_default_config() {
        log::error!("Could not create default config: {}", e);
    }

    // Initialize logger.
    SimpleLogger::new()
        .with_level(config.get_log_level().to_level_filter())
        .init()
        .unwrap();

    // Initialize job manager.
    let manager = Arc::new(Mutex::new(Manager::new()));

    // Maintain job manager, re-issuing job of died workers, etc.
    let manager_handle = Arc::clone(&manager);
    tokio::spawn(async move {
        loop {
            sleep(Duration::from_secs(5 * 60)).await;
            log::trace!("Performing job maintenance...");
            manager_handle.lock().unwrap().run_maintenance();
        }
    });

    // Start accepting incoming connections.
    let listening_on_ipv4 = spawn_listener_on(
        config.server_bind_v4.clone(),
        config.server_port.clone(),
        config.clone(),
        manager.clone(),
    );
    let listening_on_ipv6 = spawn_listener_on(
        config.server_bind_v6.clone(),
        config.server_port.clone(),
        config.clone(),
        manager.clone(),
    );

    // This will wait until one of the sockets returns with an error.
    let listening_result = try_join!(listening_on_ipv4, listening_on_ipv6);

    // If we return, either an error was thrown or both listenings were not configured and returned false.
    let (listened_on_ipv4, listened_on_ipv6) = listening_result?;

    if listened_on_ipv4 || listened_on_ipv6 {
        Ok(()) // I think, everything went okay.
    } else {
        Err("Neither IPv4 nor IPv6 bind was configured!".into())
    }
}

/// Returns true if connections are accepted on this socket.
async fn spawn_listener_on(
    address: Option<String>,
    port: u16,
    config: Config,
    manager: Arc<Mutex<Manager>>,
) -> Result<bool, Box<dyn Error>> {
    let listener = match address {
        Some(address) => TcpListener::bind(format!("{}:{}", address, port)).await?,
        None => {
            return Ok(false); // this address is not configured
        }
    };

    log::debug!("Start listening on {}...", listener.local_addr().unwrap());
    loop {
        let (stream, addr) = listener.accept().await?;
        log::trace!("New connection from {}!", addr);

        // New reference-counted pointer to job manager
        let manager = Arc::clone(&manager);
        let config = config.clone();

        tokio::spawn(async move {
            // Process each connection concurrently
            handle_connection(stream, manager, config).await;
            log::trace!("Closed connection to {}!", addr);
        });
    }

    // There is not Ok(true). This part is unreachable.
}

async fn handle_connection(stream: TcpStream, manager: Arc<Mutex<Manager>>, config: Config) {
    // Read hello message to distinguish between client and worker
    let mut stream = MessageStream::new(stream);
    match stream.receive::<HelloMessage>().await {
        Ok(HelloMessage::HelloFromClient) => {
            // Handle client connection
            match stream.send(&ServerToClientMessage::WelcomeClient).await {
                Ok(()) => {
                    log::trace!("Established connection to client!");
                    let mut client = ClientConnection::new(stream, manager, config);
                    client.run().await;
                }
                Err(e) => log::error!("Failed to send WelcomeClient: {}", e),
            }
        }
        Ok(HelloMessage::HelloFromWorker { name }) => {
            // Handle worker connection
            match stream.send(&ServerToWorkerMessage::WelcomeWorker).await {
                Ok(()) => {
                    log::trace!("Established connection to worker '{}'!", name);
                    let mut worker = WorkerConnection::new(name.clone(), stream, manager, config);
                    worker.run().await;
                }
                Err(e) => log::error!("Failed to send WelcomeWorker: {}", e),
            }
            log::warn!("Connection to worker {} closed!", name);
        }
        // Warn, because this should not be common.
        Err(e) => log::error!("Failed to read HelloMessage: {}", e),
    }
}
