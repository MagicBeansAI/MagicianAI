//! LLM job ownership over the shared bounded fair queue.
use super::job::LlmJob;
use runtime_core::fair_queue::FairJob;
pub(crate) type FairLane = runtime_core::fair_queue::FairLane<LlmJob>;
pub(crate) type TryPushError = runtime_core::fair_queue::TryPushError<LlmJob>;

/// Owner key used for round-robin inside one priority lane.
///
/// 1. `task_ref.agent_id` when `Some` and non-empty
/// 2. else `task_ref.task_id` when non-empty
/// 3. else `job.job_id` (unique, so anonymous jobs do not clump)
pub(crate) fn owner_key(job: &LlmJob) -> String {
    if let Some(task_ref) = job.task_ref.as_ref() {
        if let Some(agent_id) = task_ref.agent_id.as_ref() {
            if !agent_id.is_empty() {
                return agent_id.clone();
            }
        }
        if !task_ref.task_id.is_empty() {
            return task_ref.task_id.clone();
        }
    }
    job.job_id.as_str().to_owned()
}

impl FairJob for LlmJob {
    fn owner_key(&self) -> String {
        owner_key(self)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::dispatch::types::{JobOrigin, TaskRef};
    use crate::types::LLMRequest;

    fn job() -> LlmJob {
        let (job, _rx) = LlmJob::new(LLMRequest::default(), JobOrigin::op("fair-lane"));
        job
    }

    fn job_for_agent(agent: &str) -> LlmJob {
        job().with_task(TaskRef::task(format!("task-{agent}")).with_agent(agent))
    }

    fn job_for_task(task: &str) -> LlmJob {
        job().with_task(TaskRef::task(task))
    }

    #[test]
    fn owner_key_prefers_agent_then_task_then_job_id() {
        let with_agent = job_for_agent("agent-a");
        assert_eq!(owner_key(&with_agent), "agent-a");

        let empty_agent = job().with_task(TaskRef::task("task-only").with_agent(""));
        assert_eq!(owner_key(&empty_agent), "task-only");

        let task_only = job_for_task("task-only");
        assert_eq!(owner_key(&task_only), "task-only");

        let anonymous = job();
        let expected = anonymous.job_id.as_str().to_owned();
        assert_eq!(owner_key(&anonymous), expected);
    }

    #[test]
    fn pops_owners_round_robin_within_one_lane() {
        let lane = FairLane::new(8);
        let a1 = job_for_agent("A");
        let a1_id = a1.job_id.clone();
        let a2 = job_for_agent("A");
        let a2_id = a2.job_id.clone();
        let b1 = job_for_agent("B");
        let b1_id = b1.job_id.clone();

        assert!(lane.try_push(a1).is_ok());
        assert!(lane.try_push(a2).is_ok());
        assert!(lane.try_push(b1).is_ok());
        assert_eq!(lane.len(), 3);

        assert_eq!(lane.pop_next().expect("A1").job_id, a1_id);
        assert_eq!(lane.pop_next().expect("B1").job_id, b1_id);
        assert_eq!(lane.pop_next().expect("A2").job_id, a2_id);
        assert!(lane.pop_next().is_none());
        assert!(lane.is_empty());
    }

    #[test]
    fn try_push_returns_full_at_capacity() {
        let lane = FairLane::new(1);
        assert!(lane.try_push(job()).is_ok());
        match lane.try_push(job()) {
            Err(TryPushError::Full(_)) => {},
            Err(TryPushError::Closed(_)) => panic!("expected Full, got Closed"),
            Ok(()) => panic!("expected Full, got Ok"),
        }
        assert_eq!(lane.len(), 1);
        assert_eq!(lane.capacity(), 1);
    }

    #[tokio::test]
    async fn empty_recv_wakes_on_push() {
        let lane = FairLane::new(4);
        let recv_lane = lane.clone();
        let pushed = job_for_agent("wake");
        let pushed_id = pushed.job_id.clone();
        let handle = tokio::spawn(async move { recv_lane.recv().await });
        tokio::task::yield_now().await;
        assert!(lane.try_push(pushed).is_ok());
        let got = tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("recv should not hang")
            .expect("recv task")
            .expect("job");
        assert_eq!(got.job_id, pushed_id);
    }

    #[test]
    fn try_push_after_close_returns_closed() {
        let lane = FairLane::new(2);
        lane.close();
        match lane.try_push(job()) {
            Err(TryPushError::Closed(_)) => {},
            Err(TryPushError::Full(_)) => panic!("expected Closed, got Full"),
            Ok(()) => panic!("expected Closed, got Ok"),
        }
        assert!(lane.is_empty());
    }

    #[tokio::test]
    async fn push_after_close_returns_the_job() {
        let lane = FairLane::new(1);
        assert!(lane.try_push(job()).is_ok());
        let push_lane = lane.clone();
        let waiting = job_for_agent("closed-waiter");
        let waiting_id = waiting.job_id.clone();
        let handle = tokio::spawn(async move { push_lane.push(waiting).await });
        tokio::task::yield_now().await;
        lane.close();
        let returned = tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("push should wake on close")
            .expect("push task")
            .expect_err("closed lane must return the job");
        assert_eq!(returned.job_id, waiting_id);
    }

    #[tokio::test]
    async fn recv_returns_none_after_close() {
        let lane = FairLane::new(2);
        let recv_lane = lane.clone();
        let handle = tokio::spawn(async move { recv_lane.recv().await });
        tokio::task::yield_now().await;
        lane.close();
        let got = tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("recv should not hang")
            .expect("recv task");
        assert!(got.is_none());
    }

    #[test]
    fn three_owners_round_robin_then_continue_fifo_within_owner() {
        let lane = FairLane::new(16);
        let ids = ["A", "A", "B", "C", "A"]
            .into_iter()
            .map(|agent| {
                let job = job_for_agent(agent);
                let id = job.job_id.clone();
                assert!(lane.try_push(job).is_ok());
                id
            })
            .collect::<Vec<_>>();
        let popped = (0..5)
            .map(|_| lane.pop_next().expect("job").job_id)
            .collect::<Vec<_>>();
        assert_eq!(
            popped,
            vec![
                ids[0].clone(),
                ids[2].clone(),
                ids[3].clone(),
                ids[1].clone(),
                ids[4].clone(),
            ]
        );
    }

    #[tokio::test]
    async fn push_waits_until_pop_makes_capacity() {
        let lane = FairLane::new(1);
        assert!(lane.try_push(job()).is_ok());
        let push_lane = lane.clone();
        let waiting = job_for_agent("waiter");
        let waiting_id = waiting.job_id.clone();
        let handle = tokio::spawn(async move { push_lane.push(waiting).await });
        tokio::task::yield_now().await;
        assert_eq!(lane.len(), 1, "waiter must not have bypassed the cap");
        assert!(lane.pop_next().is_some());
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("push should not hang")
            .expect("push task")
            .unwrap_or_else(|_| panic!("lane accepted the waiter"));
        assert_eq!(lane.pop_next().expect("waiter").job_id, waiting_id);
        assert!(lane.is_empty());
    }

    #[tokio::test]
    async fn wait_not_empty_stays_pending_on_closed_empty_lane() {
        let lane = FairLane::new(2);
        let wait_lane = lane.clone();
        let handle = tokio::spawn(async move {
            wait_lane.wait_not_empty().await;
        });
        tokio::task::yield_now().await;
        lane.close();
        let woke = tokio::time::timeout(Duration::from_millis(80), handle).await;
        assert!(
            woke.is_err(),
            "closed empty lane must not complete wait_not_empty; workers exit on shutdown"
        );
    }

    #[tokio::test]
    async fn wait_not_empty_does_not_pop() {
        let lane = FairLane::new(4);
        let wait_lane = lane.clone();
        let handle = tokio::spawn(async move {
            wait_lane.wait_not_empty().await;
            wait_lane.len()
        });
        tokio::task::yield_now().await;
        assert!(lane.try_push(job_for_agent("wake")).is_ok());
        let len = tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("wait_not_empty should not hang")
            .expect("wait task");
        assert_eq!(len, 1, "wait_not_empty must leave the job in the lane");
        assert_eq!(lane.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_push_and_pop_do_not_lose_jobs() {
        let lane = FairLane::new(64);
        let n = 48usize;
        let mut pushers = Vec::with_capacity(n);
        let mut expected = Vec::with_capacity(n);
        for i in 0..n {
            let job = job_for_agent(&format!("owner-{}", i % 6));
            expected.push(job.job_id.clone());
            let push_lane = lane.clone();
            pushers.push(tokio::spawn(async move {
                push_lane
                    .push(job)
                    .await
                    .map_err(|_| "closed")
                    .expect("push")
            }));
        }
        let recv_lane = lane.clone();
        let receiver = tokio::spawn(async move {
            let mut got = Vec::with_capacity(n);
            for _ in 0..n {
                got.push(recv_lane.recv().await.expect("job").job_id);
            }
            got
        });
        for pusher in pushers {
            pusher.await.expect("pusher join");
        }
        let mut got = tokio::time::timeout(Duration::from_secs(2), receiver)
            .await
            .expect("recv should drain")
            .expect("recv join");
        got.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        expected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        assert_eq!(got, expected);
        assert!(lane.is_empty());
    }

    #[tokio::test]
    async fn worker_style_wait_then_pick_does_not_drop_a_ready_sibling_lane() {
        let high = FairLane::new(8);
        let normal = FairLane::new(8);
        let background = FairLane::new(8);
        assert!(high.try_push(job_for_agent("H")).is_ok());
        assert!(normal.try_push(job_for_agent("N")).is_ok());

        tokio::select! {
            biased;
            _ = high.wait_not_empty() => {}
            _ = normal.wait_not_empty() => {}
            _ = background.wait_not_empty() => {}
        }
        assert_eq!(high.len(), 1, "wait must not pop high");
        assert_eq!(normal.len(), 1, "wait must not pop normal");
        assert!(high.pop_next().is_some());
        assert!(normal.pop_next().is_some());
    }
}
