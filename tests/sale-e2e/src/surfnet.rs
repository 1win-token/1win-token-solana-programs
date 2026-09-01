//! Managing the surfpool process plus the surfnet_* RPC cheat codes.
//!
//! Time: `surfnet_timeTravel` in surfpool 1.0.0 fails in `--offline` mode (a
//! division by `slots_in_epoch == 0`), so the warp is implemented with the
//! `surfnet_pauseClock` + `surfnet_setAccount(Clock sysvar)` pair: with the ticker
//! stopped the Clock that was written is not overwritten, while transactions keep
//! executing (verified empirically).
//!
//! Deployment: `surfnet_writeProgram` in surfpool 1.0.0 creates the accounts, but the
//! program cache keeps a stub ("Hello from my program!") - what executes is not our
//! ELF. The working path is a genuine `solana program deploy --use-rpc` onto a
//! temporary keypair + `surfnet_cloneProgramAccount` onto declare_id +
//! `surfnet_setProgramAuthority` (the upgrade authority for initialize).

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use solana_commitment_config::CommitmentConfig;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::request::RpcRequest;

/// A live surfpool process with an RPC client for the cheat codes. Killed on Drop.
pub struct Surfnet {
    child: Child,
    pub rpc_url: String,
    rpc: RpcClient,
    pub log_path: PathBuf,
    workdir: PathBuf,
}

/// The byte layout of the Clock sysvar (bincode, 40 bytes LE).
#[derive(Debug, Clone, Copy)]
pub struct ClockView {
    pub slot: u64,
    pub epoch_start_timestamp: i64,
    pub epoch: u64,
    pub leader_schedule_epoch: u64,
    pub unix_timestamp: i64,
}

fn parse_clock(data: &[u8]) -> ClockView {
    assert!(
        data.len() >= 40,
        "the Clock sysvar is shorter than 40 bytes"
    );
    let u = |r: std::ops::Range<usize>| u64::from_le_bytes(data[r].try_into().expect("8 bytes"));
    let i = |r: std::ops::Range<usize>| i64::from_le_bytes(data[r].try_into().expect("8 bytes"));
    ClockView {
        slot: u(0..8),
        epoch_start_timestamp: i(8..16),
        epoch: u(16..24),
        leader_schedule_epoch: u(24..32),
        unix_timestamp: i(32..40),
    }
}

fn pack_clock(c: &ClockView) -> Vec<u8> {
    let mut out = Vec::with_capacity(40);
    out.extend_from_slice(&c.slot.to_le_bytes());
    out.extend_from_slice(&c.epoch_start_timestamp.to_le_bytes());
    out.extend_from_slice(&c.epoch.to_le_bytes());
    out.extend_from_slice(&c.leader_schedule_epoch.to_le_bytes());
    out.extend_from_slice(&c.unix_timestamp.to_le_bytes());
    out
}

impl Surfnet {
    /// Starts surfpool on `port` (ws = port+1) in a separate working directory
    /// (E2E_WORK_DIR or a temp one) and waits for the RPC to become ready.
    /// `--offline --no-deploy --no-studio`: fully local, deploying nothing on its own.
    pub fn start(port: u16) -> Self {
        // Any leftover of a previous unfinished run on this port is killed: otherwise a
        // repeated start would attach to a foreign instance carrying stale state.
        let _ = Command::new("pkill")
            .args(["-f", &format!("surfpool start.*--port {port}")])
            .status();
        std::thread::sleep(Duration::from_millis(300));

        let workdir = std::env::var("E2E_WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir())
            .join(format!("sale-e2e-{port}"));
        std::fs::create_dir_all(&workdir).expect("creating the surfpool working directory");
        let log_path = workdir.join("surfpool.log");
        let log = std::fs::File::create(&log_path).expect("creating surfpool.log");
        let log_err = log.try_clone().expect("dup surfpool.log");

        let child = Command::new("surfpool")
            .args([
                "start",
                "--no-tui",
                "--no-deploy",
                "--no-studio",
                "--offline",
                "-y",
                "--port",
                &port.to_string(),
                "--ws-port",
                &(port + 1).to_string(),
            ])
            .current_dir(&workdir)
            .env("NO_DNA", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .expect("starting surfpool (not in PATH?)");

        let rpc_url = format!("http://127.0.0.1:{port}");
        let rpc = RpcClient::new_with_timeout_and_commitment(
            rpc_url.clone(),
            Duration::from_secs(30),
            CommitmentConfig::processed(),
        );

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if rpc.get_version().is_ok() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "surfpool did not come up within 30s, log: {}",
                log_path.display()
            );
            std::thread::sleep(Duration::from_millis(200));
        }

        Self {
            child,
            rpc_url,
            rpc,
            log_path,
            workdir,
        }
    }

    fn cheatcode(&self, method: &'static str, params: Value) -> Value {
        self.rpc
            .send::<Value>(RpcRequest::Custom { method }, params)
            .unwrap_or_else(|e| panic!("{method} failed: {e}"))
    }

    /// Seats the program at `program_id` (declare_id) with a valid BPF Loader Upgradeable
    /// structure whose upgrade authority is `authority`: a genuine deploy onto a
    /// temporary keypair (`solana program deploy --use-rpc`, paid by `payer`), then a
    /// clone onto the target address and an authority change through the cheat codes.
    pub fn deploy_program_as(
        &self,
        program_id: &Pubkey,
        elf_path: &str,
        payer: &solana_keypair::Keypair,
        authority: &Pubkey,
    ) {
        use solana_signer::Signer;
        let tmp = solana_keypair::Keypair::new();
        let tmp_path = self.workdir.join("tmp-program-keypair.json");
        let payer_path = self.workdir.join("deploy-payer-keypair.json");
        write_keypair_file(&tmp, &tmp_path);
        write_keypair_file(payer, &payer_path);

        let out = Command::new("solana")
            .args([
                "program",
                "deploy",
                "--use-rpc",
                "--url",
                &self.rpc_url,
                "--keypair",
                payer_path.to_str().expect("utf8 path"),
                "--program-id",
                tmp_path.to_str().expect("utf8 path"),
                elf_path,
            ])
            .output()
            .expect("the solana CLI is unavailable (it is needed to deploy into surfpool)");
        assert!(
            out.status.success(),
            "solana program deploy failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        self.cheatcode(
            "surfnet_cloneProgramAccount",
            json!([tmp.pubkey().to_string(), program_id.to_string()]),
        );
        self.cheatcode(
            "surfnet_setProgramAuthority",
            json!([program_id.to_string(), authority.to_string()]),
        );
    }

    /// Stops the slot ticker: the Clock freezes, transactions keep executing, and the
    /// setAccount(Clock) that was written is not overwritten.
    pub fn pause_clock(&self) {
        self.cheatcode("surfnet_pauseClock", json!([]));
    }

    pub fn clock(&self) -> ClockView {
        let acc = self
            .rpc
            .get_account(&solana_sdk_ids::sysvar::clock::id())
            .expect("the Clock sysvar reads");
        parse_clock(&acc.data)
    }

    /// Warps `unix_timestamp` STRICTLY forward (see `warp_to` in tests/sale: rolling time
    /// back is forbidden by construction). Only valid while the ticker is stopped.
    pub fn warp_to(&self, unix_ts: i64) {
        let mut clock = self.clock();
        assert!(
            unix_ts > clock.unix_timestamp,
            "warp_to only moves forward: now = {}, target = {unix_ts}",
            clock.unix_timestamp
        );
        clock.unix_timestamp = unix_ts;
        clock.epoch_start_timestamp = unix_ts;
        self.cheatcode(
            "surfnet_setAccount",
            json!([
                solana_sdk_ids::sysvar::clock::id().to_string(),
                { "data": hex_encode(&pack_clock(&clock)) }
            ]),
        );
        assert_eq!(
            self.clock().unix_timestamp,
            unix_ts,
            "the Clock sysvar did not accept the warp (is the ticker not stopped?)"
        );
    }
}

impl Drop for Surfnet {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// A keypair in the solana-keygen file format (a JSON array of 64 bytes).
fn write_keypair_file(kp: &solana_keypair::Keypair, path: &std::path::Path) {
    let bytes: Vec<u8> = kp.to_bytes().to_vec();
    std::fs::write(path, serde_json::to_string(&bytes).expect("json"))
        .expect("writing the keypair file");
}
