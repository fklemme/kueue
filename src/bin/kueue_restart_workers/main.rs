use anyhow::{anyhow, Result};
use kueue::config::Config;
use simple_logger::SimpleLogger;
use ssh2::Session;
use std::{io::Read, net::TcpStream, thread::sleep, time::Duration};

fn main() -> Result<()> {
    // Read configuration from file or defaults.
    let config = Config::new().map_err(|e| anyhow!("Failed to load config: {}", e))?;
    let restart_workers = config
        .restart_workers
        .clone()
        .expect("[restart_workers] missing!");
    let ssh_user = restart_workers.ssh_user;
    let workers: Vec<_> = restart_workers.hostnames.split_whitespace().collect();
    let sleep_duration =
        Duration::from_secs_f64(restart_workers.sleep_minutes_before_recheck * 60.0);

    // Initialize logger.
    SimpleLogger::new()
        .with_level(config.get_log_level().to_level_filter())
        .init()
        .unwrap();

    loop {
        for worker in &workers {
            if let Err(e) = process_worker(worker, &ssh_user) {
                log::error!("Failed processing worker {}: {}", worker, e);
            }
        }
        sleep(sleep_duration);
    }
}

fn process_worker(worker: &str, ssh_user: &str) -> Result<()> {
    log::trace!("Processing worker {}...", worker);

    // TODO: Requires some kind of "ssh-add ~/.ssh/id_rsa"
    //       If agent not started: "eval `ssh-agent -s`"
    let mut session = Session::new()?;

    // Connect to worker
    let tcp_stream = TcpStream::connect(format!("{}:{}", worker, 22))?;
    session.set_tcp_stream(tcp_stream);
    session.handshake()?;
    session.userauth_agent(ssh_user)?;

    // Check if worker is running (in screen session)
    let mut screen_ls = String::new();
    let mut channel = session.channel_session()?;
    channel.exec("screen -ls")?;
    channel.read_to_string(&mut screen_ls)?;
    channel.wait_close()?;

    if screen_ls.contains("kueue_worker") {
        log::info!("Worker {} appears to be running.", worker);
    } else {
        log::warn!("Worker {} appears to be down! Restart...", worker);

        // Restart worker in detached screen
        let cmd = "screen -dmS kueue_worker bash -c kueue_worker";
        let mut output = String::new();
        let mut channel = session.channel_session()?;
        channel.exec(cmd)?;
        channel.read_to_string(&mut output)?;
        channel.wait_close()?;

        log::debug!("Output after restarting: {}", output)
    }

    Ok(()) // done
}