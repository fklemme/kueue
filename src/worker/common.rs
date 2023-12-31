use crate::{
    config::Config,
    messages::stream::{MessageStream, MessageError},
    messages::{HelloMessage, ServerToWorkerMessage, WorkerToServerMessage},
    worker::job::Job, structs::{JobInfo, Resources, LoadInfo, SystemInfo, JobStatus},
};
use anyhow::{bail, Result};
use std::{cmp::{max, min},sync::Arc};
use sysinfo::{CpuExt, System, SystemExt};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc::Sender, Notify},
};
use tokio_util::sync::CancellationToken;
use base64::{engine::general_purpose, Engine};
use sha2::{Digest, Sha256};
use chrono::Utc;

pub struct Worker<Stream> {
    /// Settings from the parsed config file.
    config: Config,
    /// Name of the worker. Used as an identifier.
    worker_name: String,
    stream: MessageStream<Stream>,
    cancel_token: CancellationToken,
    keep_alive: Sender<()>,
    /// Handle to query system information.
    system_info: System,
    /// Regularly notified by a timer to trigger sending a message
    /// about updated system and hardware information to the server.
    pub notify_system_update: Arc<Notify>,
    /// Notified, whenever any job thread concludes.
    /// The job's status has already been updated by this time.
    notify_job_status: Arc<Notify>,
    /// Jobs accepted by the worker but not yet confirmed or started.
    accepted_jobs: Vec<Job>,
    /// Jobs currently running on the worker.
    running_jobs: Vec<Job>,
    /// State of the worker.
    running: bool,
}

impl<Stream: AsyncReadExt + AsyncWriteExt + Unpin> Worker<Stream> {
    pub fn new(
        config: Config,
        worker_name: String,
        stream: MessageStream<Stream>,
        cancel_token: CancellationToken,
        keep_alive: Sender<()>,
    ) -> Self {
        // Initialize system resources.
        let system_info = System::new_all();

        Self {
            config,
            worker_name,
            stream,
            cancel_token,
            keep_alive,system_info,
            notify_system_update: Arc::new(Notify::new()),
            notify_job_status: Arc::new(Notify::new()),
            accepted_jobs: Vec::new(),
            running_jobs: Vec::new(),
            running: true,
        }
    }

    /// Perform hello/welcome handshake with the server.
    pub async fn connect_to_server(&mut self) -> Result<()> {
        // Send hello from worker.
        let hello = HelloMessage::HelloFromWorker {
            worker_name: self.worker_name.clone(),
        };
        self.stream.send(&hello).await?;

        // Await welcoming response from server.
        match self.stream.receive::<ServerToWorkerMessage>().await? {
            ServerToWorkerMessage::WelcomeWorker => {
                log::trace!("Established connection to server...");
                Ok(()) // continue
            }
            other => bail!("Expected WelcomeWorker, received: {:?}", other),
        }
    }

    /// Perform challenge-response authentication. We only need to answer the
    /// challenge without waiting for a response. If the authentication fails,
    /// the server closes the connection.
    pub async fn authenticate(&mut self) -> Result<()> {
        // Authentication challenge is sent automatically after welcome.
        match self.stream.receive::<ServerToWorkerMessage>().await? {
            ServerToWorkerMessage::AuthChallenge { salt } => {
                // Calculate response.
                let salted_secret = self.config.common_settings.shared_secret.clone() + &salt;
                let salted_secret = salted_secret.into_bytes();
                let mut hasher = Sha256::new();
                hasher.update(salted_secret);
                let response = hasher.finalize().to_vec();
                let response = general_purpose::STANDARD_NO_PAD.encode(response);

                // Send response back to server.
                let message = WorkerToServerMessage::AuthResponse(response);
                self.stream.send(&message).await?;
            }
            other => {
                bail!("Expected AuthChallenge, received: {:?}", other);
            }
        }

        // Await authentication confirmation.
        match self.stream.receive::<ServerToWorkerMessage>().await? {
            ServerToWorkerMessage::AuthAccepted(accepted) => {
                if accepted {
                    Ok(())
                } else {
                    bail!("Authentication failed!")
                }
            }
            other => bail!("Expected AuthAccepted, received: {:?}", other),
        }
    }

    /// Handle messages and interrupts.
    pub async fn run(&mut self) -> Result<(), MessageError> {
        while self.running {
            tokio::select! {
                // Read and handle incoming messages.
                message = self.stream.receive::<ServerToWorkerMessage>() => {
                    self.handle_message(message?).await?;
                }
                // Or, get active when notified by timer.
                _ = self.notify_system_update.notified() => {
                    // Update server about system and load.
                    // Also used as "keep alive" signal by the server.
                    self.update_system_info().await?;
                    // Also send an update on available resources. This is
                    // important when "dynamic resources" are used. Otherwise,
                    // the server might see an outdated, full-loaded worker
                    // with no running jobs and will never offer any new jobs.
                    let message = WorkerToServerMessage::UpdateResources(self.get_available_resources());
                    self.stream.send(&message).await?;
                }
                // Or, get active when a job finishes.
                _ = self.notify_job_status.notified() => {
                    self.update_job_status().await?;
                }
                // Or, close the connection if worker is shutting down.
                _ = self.cancel_token.cancelled() => {
                    log::info!("Closing connection to server!");
                    if let Err(e) = self.stream.send(&WorkerToServerMessage::Bye).await {
                        log::error!("Failed to send bye: {}", e);
                    }
                    self.running = false; // stop worker
                }
            }
        }
        Ok(())
    }

    /// Called in the main loop to handle different incoming messages from the server.
    pub async fn handle_message(&mut self, message: ServerToWorkerMessage) -> Result<(), MessageError> {
        match message {
            ServerToWorkerMessage::WelcomeWorker => {
                // This is already handled before the main loop begins.
                log::warn!("Received duplicate welcome message!");
                Ok(())
            }
            ServerToWorkerMessage::AuthChallenge { salt: _ } => {
                // This is already handled before the main loop begins.
                log::warn!("Received duplicate authentication challenge!");
                Ok(())
            }
            ServerToWorkerMessage::AuthAccepted(_accepted) => {
                // This is already handled before the main loop begins.
                log::warn!("Received duplicate authentication acceptance!");
                Ok(())
            }
            ServerToWorkerMessage::OfferJob(job_info) => self.on_offer_job(job_info).await,
            ServerToWorkerMessage::ConfirmJobOffer(job_info) => {
                self.on_confirm_job_offer(job_info).await
            }
            ServerToWorkerMessage::WithdrawJobOffer(job_info) => {
                self.on_withdraw_job_offer(job_info).await
            }
            ServerToWorkerMessage::KillJob(job_info) => self.on_kill_job(job_info).await,
            ServerToWorkerMessage::Bye => {
                log::debug!("Connection closed by server!");
                self.running = false; // stop worker
                Ok(())
            }
        }
    }

    /// Called upon receiving ServerToWorkerMessage::OfferJob.
    async fn on_offer_job(&mut self, job_info: JobInfo) -> Result<(), MessageError> {
        // Reject job when the worker cannot see the working directory.
        if !job_info.cwd.is_dir() {
            log::debug!(
                "Rejected job {} because working directory {} is not found!",
                job_info.job_id,
                job_info.cwd.to_string_lossy()
            );

            // Reject job offer.
            return self
                .stream
                .send(&WorkerToServerMessage::RejectJobOffer(job_info))
                .await;
        }

        // Accept job if required resources can be acquired.
        if self.resources_available(&job_info.worker_resources) {
            log::debug!("Accepted job {}!", job_info.job_id);

            // Remember accepted job for later confirmation.
            self.accepted_jobs.push(Job::new(
                job_info.clone(),
                Arc::clone(&self.notify_job_status),
            ));
            // Notify server about accepted job offer.
            self.stream
                .send(&WorkerToServerMessage::AcceptJobOffer(job_info))
                .await
        } else {
            log::debug!("Deferred job {}!", job_info.job_id);

            // Defer job offer (until resources become available).
            self.stream
                .send(&WorkerToServerMessage::DeferJobOffer(job_info))
                .await
        }
    }

    /// Called upon receiving ServerToWorkerMessage::ConfirmJobOffer.
    async fn on_confirm_job_offer(&mut self, job_info: JobInfo) -> Result<(), MessageError> {
        let accepted_job_index = self
            .accepted_jobs
            .iter()
            .position(|job| job.info.job_id == job_info.job_id);
        match accepted_job_index {
            Some(index) => {
                // Move job to running processes.
                let mut job = self.accepted_jobs.remove(index);
                // Also update job status. (Should now be "running".)
                job.info.status = job_info.status;
                self.running_jobs.push(job);

                // TODO: Also compare entire "job_info"s for consistency?

                // Run job as child process
                match self.running_jobs.last_mut().unwrap().run().await {
                    Ok(()) => {
                        log::debug!("Started job {}!", job_info.job_id);

                        // Inform server about available resources.
                        // This information triggers the server to
                        // send new job offers to this worker.
                        let message =
                            WorkerToServerMessage::UpdateResources(self.get_available_resources());
                        self.stream.send(&message).await
                    }
                    Err(e) => {
                        log::error!("Failed to start job: {}", e);
                        let job = self.running_jobs.last_mut().unwrap();
                        {
                            let mut job_result = job.result.lock().unwrap();
                            job_result.finished = true;
                            job_result.exit_code = -43;
                            job_result.run_time = chrono::Duration::seconds(0);
                            job_result.comment = format!("Failed to start job: {}", e);
                        }

                        // Update server. This will also send an update on
                        // available resources and thus trigger new job offers.
                        self.update_job_status().await
                    }
                }

                // TODO: There is some checking we could do here. We do
                // not want jobs to remain in accepted_jobs indefinitely!
            }
            None => {
                log::error!(
                    "Confirmed job with ID={} that has not been offered previously!",
                    job_info.job_id
                );
                Ok(()) // Error occurred, but we can continue running.
            }
        }
    }

    /// Called upon receiving ServerToWorkerMessage::WithdrawJobOffer.
    async fn on_withdraw_job_offer(&mut self, job_info: JobInfo) -> Result<(), MessageError> {
        // Remove job from offered list
        let accepted_job_index = self
            .accepted_jobs
            .iter()
            .position(|job| job.info.job_id == job_info.job_id);
        match accepted_job_index {
            Some(index) => {
                // Remove job from list
                let job = self.accepted_jobs.remove(index);
                log::debug!("Withdrawn job: {:?}", job);
                Ok(())
            }
            None => {
                log::error!(
                    "Withdrawn job with ID={} that has not been offered previously!",
                    job_info.job_id
                );
                Ok(()) // Error occurred, but we can continue running.
            }
        }
    }

    /// Called upon receiving ServerToWorkerMessage::KillJob.
    async fn on_kill_job(&mut self, job_info: JobInfo) -> Result<(), MessageError> {
        let running_job_index = self
            .running_jobs
            .iter()
            .position(|job| job.info.job_id == job_info.job_id);
        match running_job_index {
            Some(index) => {
                // Signal kill.
                let job = self.running_jobs.get_mut(index).unwrap();
                job.notify_kill_job.notify_one();
                // Also update job status. (Should now be "canceled".)
                job.info.status = job_info.status;
                // After the job has been killed, "notify_job_status"
                // is notified, causing "update_job_status" to remove
                // the job from "running_jobs" and inform the server.
                Ok(())
            }
            None => {
                log::error!(
                    "Job to be killed with ID={} is not running on this worker!",
                    job_info.job_id
                );
                Ok(()) // Error occurred, but we can continue running.
            }
        }
    }

    /// Returns available, unused resources of the worker.
    fn get_available_resources(&mut self) -> Resources {
        // Refresh relevant system information.
        self.system_info.refresh_cpu();
        self.system_info.refresh_memory();

        // Calculate available job slots.
        let allocated_job_slots: u64 = self
            .accepted_jobs
            .iter()
            .chain(self.running_jobs.iter())
            .map(|job| job.info.worker_resources.job_slots)
            .sum();
        assert!(self.config.worker_settings.worker_max_parallel_jobs >= allocated_job_slots);
        let available_job_slots =
            self.config.worker_settings.worker_max_parallel_jobs - allocated_job_slots;

        // Calculate available cpus.
        let total_cpus = self.system_info.cpus().len() as i64;

        let allocated_cpus: i64 = self
            .accepted_jobs
            .iter()
            .chain(self.running_jobs.iter())
            .map(|job| job.info.worker_resources.cpus as i64)
            .sum();

        let available_cpus = if self.config.worker_settings.dynamic_check_free_resources {
            let busy_cpus = self.system_info.load_average().one.ceil() as i64;

            // TODO: Is this calculation better? Maybe working on Windows as well?
            // let busy_cpus = self
            //     .system_info
            //     .cpus()
            //     .iter()
            //     .map(|cpu| cpu.cpu_usage())
            //     .sum::<f32>();
            // let busy_cpus = (busy_cpus / 100.0 / total_cpus as f32).ceil() as i64;

            let busy_cpus = (busy_cpus as f64
                * self.config.worker_settings.dynamic_cpu_load_scale_factor)
                .ceil() as i64;
            max(0, total_cpus - max(allocated_cpus, busy_cpus))
        } else {
            max(0, total_cpus - allocated_cpus)
        };

        // Calculate available memory.
        let total_ram_mb = (self.system_info.total_memory() / 1024 / 1024) as i64;

        let allocated_ram_mb: i64 = self
            .accepted_jobs
            .iter()
            .chain(self.running_jobs.iter())
            .map(|job| job.info.worker_resources.ram_mb as i64)
            .sum();

        let available_ram_mb = if self.config.worker_settings.dynamic_check_free_resources {
            let available_ram_mb = (self.system_info.available_memory() / 1024 / 1024) as i64;
            max(0, min(total_ram_mb - allocated_ram_mb, available_ram_mb))
        } else {
            max(0, total_ram_mb - allocated_ram_mb)
        };

        Resources::new(
            available_job_slots,
            available_cpus as u64,
            available_ram_mb as u64,
        )
    }

    /// Returns "true" if there are enough resources free to
    /// fit the demand of the given "required" resources.
    fn resources_available(&mut self, required: &Resources) -> bool {
        let available = self.get_available_resources();
        required.fit_into(&available)
    }

    /// Update and report hardware information and system load.
    async fn update_system_info(&mut self) -> Result<(), MessageError> {
        // Refresh relevant system information.
        self.system_info.refresh_cpu();
        self.system_info.refresh_memory();

        // Get CPU cores, frequency, and RAM.
        let cpu_cores = self.system_info.cpus().len() as u64;
        let cpu_frequency = if cpu_cores > 0 {
            self.system_info
                .cpus()
                .iter()
                .map(|cpu| cpu.frequency())
                .sum::<u64>()
                / cpu_cores
        } else {
            0
        };
        let total_ram_mb = self.system_info.total_memory() / 1024 / 1024;

        // Read system load.
        let load_avg = self.system_info.load_average();
        let load_info = LoadInfo {
            one: load_avg.one,
            five: load_avg.five,
            fifteen: load_avg.fifteen,
        };

        // Collect hardware information.
        let system_info = SystemInfo {
            kernel: self
                .system_info
                .kernel_version()
                .unwrap_or("unknown".into()),
            distribution: self
                .system_info
                .long_os_version()
                .unwrap_or("unknown".into()),
            cpu_cores,
            cpu_frequency,
            total_ram_mb,
            load_info,
        };

        // Send to server.
        self.stream
            .send(&WorkerToServerMessage::UpdateSystemInfo(system_info))
            .await
    }

    /// Find jobs that have concluded and update the server about the new status.
    async fn update_job_status(&mut self) -> Result<(), MessageError> {
        // We check all running processes for exit codes
        let mut index = 0;
        while index < self.running_jobs.len() {
            let finished = self.running_jobs[index].result.lock().unwrap().finished;
            if finished {
                // Job has finished. Remove from list.
                let mut job = self.running_jobs.remove(index);
                let mut stdout_text = None;
                let mut stderr_text = None;

                {
                    // Update info
                    let result_lock = job.result.lock().unwrap();
                    match job.info.status {
                        JobStatus::Running {
                            issued, started, ..
                        } => {
                            job.info.status = JobStatus::Finished {
                                issued,
                                started,
                                finished: Utc::now(),
                                return_code: result_lock.exit_code,
                                worker: self.worker_name.clone(),
                                run_time_seconds: result_lock.run_time.num_seconds(),
                                comment: result_lock.comment.clone(),
                            };
                        }
                        JobStatus::Canceled { .. } => {} // leave status as it is (canceled)
                        _ => log::error!(
                            "Expected job status to be running or canceled. Found: {:?}",
                            job.info.status
                        ),
                    }
                    if !result_lock.stdout_text.is_empty() {
                        stdout_text = Some(result_lock.stdout_text.clone());
                    }
                    if !result_lock.stderr_text.is_empty() {
                        stderr_text = Some(result_lock.stderr_text.clone());
                    }
                }

                // Send update to server
                let job_status = WorkerToServerMessage::UpdateJobStatus(job.info.clone());
                self.stream.send(&job_status).await?;

                // Send stdout/stderr to server
                let job_results = WorkerToServerMessage::UpdateJobResults {
                    job_id: job.info.job_id,
                    stdout_text,
                    stderr_text,
                };
                self.stream.send(&job_results).await?;

                // Inform server about available resources. This information
                // triggers the server to send new job offers to the worker.
                let message =
                    WorkerToServerMessage::UpdateResources(self.get_available_resources());
                self.stream.send(&message).await?;

                // TODO: Store/remember finished jobs?
            } else {
                // Job still running... Next!
                index += 1;
            }
        }
        Ok(())
    }
}
