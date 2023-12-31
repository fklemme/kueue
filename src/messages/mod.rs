//! Contains a collection of structs that are transferred as messages between
//! client and server, and worker and server.

pub mod stream;

use std::collections::BTreeMap;

use crate::structs::{JobInfo, Resources, SystemInfo, WorkerInfo};
use serde::{Deserialize, Serialize};

/// Communication to the server is initialized with HelloFromClient or
/// HelloFromWorker. The variants help the server to distinguish between client
/// and worker connections. The server will respond with the corresponding
/// "welcome" message.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub enum HelloMessage {
    /// Initiate a new client connection with the HelloFromClient message.
    /// The server confirms the connection with the WelcomeClient message.
    HelloFromClient,
    /// Initiate a new worker connection with the HelloFromWorker message.
    /// The server confirms the connection with the WelcomeWorker message.
    HelloFromWorker {
        /// Name of the worker. Can be helpful for the
        /// user to identify where jobs are running.
        worker_name: String,
    },
}

/// Contains all messages sent by the client to the server.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub enum ClientToServerMessage {
    /// Request authentication challenge. This is required to issue or remove
    /// jobs. The server will reply with a AuthChallenge that must answered
    /// with a corresponding AuthResponse message.
    AuthRequest,
    /// Send response in the form of "Base64(Sha256(secret + salt))" back to the
    /// server. The server responds with a AuthAccepted(bool) to indicate if the
    /// authentication was successful.
    AuthResponse(String),
    /// Issue a new job. The job's ID and status will be ignored by the server.
    /// The server responds with a AcceptJob message and provide updated
    /// details. This command requires authentication.
    IssueJob(JobInfo),
    ListJobs {
        num_jobs: u64,
        pending: bool,
        offered: bool,
        running: bool,
        succeeded: bool,
        failed: bool,
        canceled: bool,
    },
    ShowJob {
        job_id: u64,
    },
    ObserveJob {
        job_id: u64,
    },
    RemoveJob {
        job_id: u64,
        kill: bool,
    },
    CleanJobs {
        all: bool,
    },
    ListWorkers,
    ShowWorker {
        worker_id: u64,
    },
    ListResources,
    Bye,
}

/// Contains all messages sent by the server to a client.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub enum ServerToClientMessage {
    /// Respond with WelcomeClient after HelloFromClient.
    WelcomeClient,
    /// AuthChallenge sends a random salt to the client.
    AuthChallenge {
        salt: String,
    },
    /// Let client know if authentication succeeded.
    AuthAccepted(bool),
    AcceptJob(JobInfo),
    RejectJob {
        job_info: JobInfo,
        reason: String,
    },
    JobList {
        job_infos: Vec<JobInfo>,
        jobs_pending: u64,
        jobs_offered: u64,
        jobs_running: u64,
        jobs_succeeded: u64,
        jobs_failed: u64,
        jobs_canceled: u64,
        job_avg_run_time_seconds: i64,
        remaining_jobs_eta_seconds: i64,
    },
    JobInfo {
        job_info: JobInfo,
        stdout_text: Option<String>,
        stderr_text: Option<String>,
    },
    JobUpdated(JobInfo),
    WorkerList(Vec<WorkerInfo>),
    WorkerInfo(WorkerInfo),
    ResourceList {
        used_resources: Option<BTreeMap<String, u64>>,
        total_resources: Option<BTreeMap<String, u64>>,
    },
    /// Generic response, signaling the client if the requested action has
    /// succeeded or if something went wrong. This is used, for instance, when
    /// the client requests information about a job that does not exist.
    RequestResponse {
        success: bool,
        text: String,
    },
    /// Close connection to the client. The server will only actively
    /// close the connection when the server is shutting down.
    Bye,
}

/// Contains all messages sent by the worker to the server.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub enum WorkerToServerMessage {
    /// Send Sha256(secret + salt) back to server.
    AuthResponse(String),
    /// Update hardware information and system load.
    UpdateSystemInfo(SystemInfo),
    UpdateJobStatus(JobInfo),
    UpdateJobResults {
        job_id: u64,
        stdout_text: Option<String>,
        stderr_text: Option<String>,
    },
    // Update server about available resources on the worker. The worker
    // might reply with new job offers based on the provided information.
    UpdateResources(Resources),
    AcceptJobOffer(JobInfo),
    DeferJobOffer(JobInfo),
    RejectJobOffer(JobInfo),
    Bye,
}

/// Contains all messages sent by the server to a worker.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub enum ServerToWorkerMessage {
    // Respond with WelcomeWorker after HelloFromWorker
    WelcomeWorker,
    // AuthChallenge sends a random salt to the client.
    AuthChallenge {
        salt: String,
    },
    /// Let worker know if authentication succeeded.
    AuthAccepted(bool),
    OfferJob(JobInfo),
    ConfirmJobOffer(JobInfo),
    WithdrawJobOffer(JobInfo),
    KillJob(JobInfo),
    /// Close connection to the worker. The server will only actively
    /// close the connection when the server is shutting down.
    Bye,
}
