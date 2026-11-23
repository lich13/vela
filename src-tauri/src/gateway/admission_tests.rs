//! Endless-mode retry pacing: how long a retry waits and when a crowd goes out.
use super::*;
use crate::gateway::test_support::{policy_provider, route};

const PROVIDER: &str = "fixture-provider";

/// An endless provider with the default 33-second risk-control interval,
/// which a rate limit does not rest.
fn paced(limit: Option<u64>, intensity: u8) -> Provider {
    Provider {
        endless_mode: true,
        upstream_retry_seconds: 33,
        limit_retry_seconds: limit,
        limit_rest_seconds: Some(0),
        retry_intensity: Some(intensity),
        ..policy_provider(PROVIDER, false, false)
    }
}

fn scheduler(provider: &Provider) -> Scheduler {
    let scheduler = Scheduler::new(broadcast::channel(32).0);
    scheduler.configure(std::slice::from_ref(provider), true);
    scheduler
}

/// Admits `count` requests, the way a crowd is in flight before it is blocked.
async fn admitted(scheduler: &Scheduler, provider: &Provider, count: usize) -> Vec<Admission> {
    let route = route(provider.clone());
    let mut crowd = Vec::new();
    for _ in 0..count {
        let mut budget = Budget::new(30);
        crowd.push(
            scheduler
                .acquire(std::slice::from_ref(&route), false, 64, &mut budget)
                .await
                .unwrap(),
        );
    }
    crowd
}

/// Blocks an admitted request: it registers its retry, then gives up its slot.
fn blocked(scheduler: &Scheduler, admission: Admission, class: RetryClass) -> (Retry, Duration) {
    let retry = scheduler.retry(
        &scheduler.capacity_source(PROVIDER),
        class,
        class == RetryClass::Limit,
    );
    drop(admission);
    retry
}

/// The wait of the first request blocked among `load` in flight.
async fn first_wait(provider: &Provider, load: usize, class: RetryClass) -> Duration {
    let scheduler = scheduler(provider);
    let mut crowd = admitted(&scheduler, provider, load).await;
    blocked(&scheduler, crowd.pop().unwrap(), class).1
}

/// Waits the retry out and reports how long that took.
async fn timed(retry: Retry) -> (Result<(), Rejected>, Duration) {
    let started = Instant::now();
    (retry.wait().await, started.elapsed())
}

fn rounded(delay: Duration) -> u64 {
    delay.as_secs_f64().round() as u64
}

fn tenths(delay: Duration) -> u64 {
    (delay.as_secs_f64() * 10.0).round() as u64
}

fn near(delay: Duration, milliseconds: u64) -> bool {
    delay.abs_diff(Duration::from_millis(milliseconds)) < Duration::from_millis(1)
}

#[tokio::test(start_paused = true)]
async fn a_paced_retry_waits_by_how_many_requests_press_on_the_provider() {
    // Requests in flight, then the tenths of a second waited on the default
    // interval at intensity 5 and at 10.
    for (load, reference, squared) in [
        (1, 66, 13),
        (2, 66, 13),
        (3, 73, 16),
        (4, 147, 65),
        (5, 220, 147),
        (6, 293, 261),
        (7, 367, 407),
        (8, 440, 587),
        (9, 513, 799),
        (10, 550, 917),
        (16, 550, 917),
        (25, 550, 917),
        // A crowd keeps its retries a fifteenth of the interval apart.
        (30, 660, 1320),
        (60, 1320, 5280),
    ] {
        for (intensity, expected) in [(5, reference), (10, squared)] {
            let delay = first_wait(&paced(None, intensity), load, RetryClass::Risk).await;
            assert_eq!(
                tenths(delay),
                expected,
                "load {load}, intensity {intensity}"
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn each_class_of_retry_keeps_its_own_interval() {
    // Eight in flight stretch a paced interval by a third at intensity 5.
    let provider = paced(Some(15), 5);
    assert_eq!(
        tenths(first_wait(&provider, 8, RetryClass::Risk).await),
        440
    );
    assert_eq!(
        tenths(first_wait(&provider, 8, RetryClass::Limit).await),
        200
    );
    assert_eq!(
        first_wait(&provider, 8, RetryClass::Normal).await,
        Duration::from_secs(3),
        "an ordinary retry is not paced"
    );
    assert_eq!(
        tenths(first_wait(&paced(None, 5), 8, RetryClass::Limit).await),
        440,
        "an unset rate-limit interval follows the risk-control one"
    );
    let brief = Provider {
        upstream_retry_seconds: 2,
        ..paced(None, 5)
    };
    assert_eq!(
        first_wait(&brief, 1, RetryClass::Risk).await,
        Duration::from_secs(1),
        "a lone request's fifth of the interval stops at a second"
    );
}

#[tokio::test(start_paused = true)]
async fn every_request_in_flight_or_waiting_on_the_provider_adds_to_the_pressure() {
    let provider = paced(None, 5);
    let scheduler = scheduler(&provider);
    let mut crowd = admitted(&scheduler, &provider, 6).await;
    let limited = crowd.pop().unwrap();
    // Two are still being answered and three wait out an ordinary retry.
    let ordinary: Vec<_> = crowd
        .drain(..3)
        .map(|admission| blocked(&scheduler, admission, RetryClass::Normal))
        .collect();
    let (retry, delay) = blocked(&scheduler, limited, RetryClass::Limit);
    assert_eq!(tenths(delay), 293, "six press on the provider");
    drop((retry, ordinary, crowd));

    // With nobody left a newcomer is a lone request again.
    let mut late = admitted(&scheduler, &provider, 1).await;
    let (_retry, delay) = blocked(&scheduler, late.pop().unwrap(), RetryClass::Limit);
    assert_eq!(tenths(delay), 66);
}

#[tokio::test(start_paused = true)]
async fn a_crowd_blocked_together_returns_one_at_a_time() {
    let provider = paced(None, 5);
    let scheduler = scheduler(&provider);
    let retries: Vec<_> = admitted(&scheduler, &provider, 10)
        .await
        .into_iter()
        .map(|admission| blocked(&scheduler, admission, RetryClass::Risk))
        .collect();
    // Ten stretch the interval to 55 seconds, and each goes a tenth of that
    // after the one before.
    for (place, (_, delay)) in retries.iter().enumerate() {
        assert!(
            near(*delay, 55_000 + 5_500 * place as u64),
            "place {place} waits {delay:?}"
        );
    }
    assert_eq!(scheduler.counts().1, 10);

    let waits: Vec<_> = retries
        .into_iter()
        .map(|(retry, delay)| (tokio::spawn(timed(retry)), delay))
        .collect();
    for (wait, delay) in waits {
        let (result, waited) = wait.await.unwrap();
        assert_eq!(result, Ok(()));
        // The timer wakes on the millisecond after the release.
        assert!(
            waited >= delay && waited - delay <= Duration::from_millis(1),
            "waited {waited:?} of {delay:?}"
        );
    }
    assert_eq!(scheduler.counts().1, 0);
}

#[tokio::test(start_paused = true)]
async fn intensity_zero_keeps_the_plain_interval_for_everyone() {
    let provider = paced(None, 0);
    let scheduler = scheduler(&provider);
    let retries: Vec<_> = admitted(&scheduler, &provider, 12)
        .await
        .into_iter()
        .map(|admission| blocked(&scheduler, admission, RetryClass::Risk))
        .collect();
    for (_, delay) in &retries {
        assert_eq!(*delay, Duration::from_secs(33));
    }
}

#[tokio::test(start_paused = true)]
async fn the_two_paced_classes_take_turns_in_one_line() {
    let provider = paced(None, 5);
    let scheduler = scheduler(&provider);
    let classes = [RetryClass::Risk, RetryClass::Limit];
    let delays: Vec<_> = admitted(&scheduler, &provider, 8)
        .await
        .into_iter()
        .zip(classes.into_iter().cycle())
        .map(|(admission, class)| blocked(&scheduler, admission, class))
        .collect();
    // Eight wait 44 seconds; each goes an eighth of that after the last.
    for (place, (_, delay)) in delays.iter().enumerate() {
        assert!(
            near(*delay, 44_000 + 5_500 * place as u64),
            "place {place} waits {delay:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_short_interval_is_not_held_behind_a_long_one() {
    let provider = paced(Some(15), 5);
    let scheduler = scheduler(&provider);
    let mut crowd = admitted(&scheduler, &provider, 8).await;
    let limited = crowd.pop().unwrap();
    let risk: Vec<_> = crowd
        .into_iter()
        .map(|admission| blocked(&scheduler, admission, RetryClass::Risk))
        .collect();
    assert!(near(risk[6].1, 77_000));
    tokio::time::advance(Duration::from_secs(1)).await;
    // Its twenty seconds end long before the first risk-control retry goes out.
    let (_retry, delay) = blocked(&scheduler, limited, RetryClass::Limit);
    assert!(near(delay, 20_000), "waits {delay:?}");
}

#[tokio::test(start_paused = true)]
async fn an_edited_pace_moves_a_retry_that_is_already_waiting() {
    // Eight in flight stretch the interval by a third at intensity 5. Twenty
    // seconds into the wait the provider is saved with the pace on the left;
    // the retry then goes out that many seconds after its failure.
    for (edited, expected) in [
        (
            Provider {
                upstream_retry_seconds: 18,
                ..paced(None, 5)
            },
            24,
        ),
        // Shorter than what it has already waited: at once.
        (
            Provider {
                upstream_retry_seconds: 6,
                ..paced(None, 5)
            },
            20,
        ),
        (
            Provider {
                upstream_retry_seconds: 60,
                ..paced(None, 5)
            },
            80,
        ),
        (paced(None, 10), 59),
        (paced(None, 0), 33),
    ] {
        let provider = paced(None, 5);
        let scheduler = scheduler(&provider);
        let mut crowd = admitted(&scheduler, &provider, 8).await;
        let (retry, delay) = blocked(&scheduler, crowd.pop().unwrap(), RetryClass::Risk);
        assert_eq!(rounded(delay), 44);
        let waiting = tokio::spawn(timed(retry));
        tokio::time::sleep(Duration::from_secs(20)).await;
        scheduler.configure(std::slice::from_ref(&edited), true);
        assert_eq!(
            scheduler.transient_retries()[0].retry_in,
            expected - 20,
            "the countdown follows the edit"
        );
        let (result, waited) = waiting.await.unwrap();
        assert_eq!(result, Ok(()));
        assert_eq!(rounded(waited), expected);
    }
}

#[tokio::test(start_paused = true)]
async fn retries_overdue_after_an_edit_still_go_out_one_at_a_time() {
    let provider = paced(None, 5);
    let scheduler = scheduler(&provider);
    let waits: Vec<_> = admitted(&scheduler, &provider, 8)
        .await
        .into_iter()
        .map(|admission| tokio::spawn(timed(blocked(&scheduler, admission, RetryClass::Risk).0)))
        .collect();
    tokio::time::sleep(Duration::from_secs(40)).await;
    let edited = Provider {
        upstream_retry_seconds: 6,
        ..paced(None, 5)
    };
    scheduler.configure(std::slice::from_ref(&edited), true);
    // All eight are past their eight seconds; they leave a second apart.
    for (place, wait) in waits.into_iter().enumerate() {
        let (result, waited) = wait.await.unwrap();
        assert_eq!(result, Ok(()));
        assert_eq!(rounded(waited), 40 + place as u64);
    }
}

#[tokio::test(start_paused = true)]
async fn a_retry_that_is_given_up_frees_its_place() {
    let provider = paced(None, 5);
    let scheduler = scheduler(&provider);
    let retries: Vec<_> = admitted(&scheduler, &provider, 10)
        .await
        .into_iter()
        .map(|admission| blocked(&scheduler, admission, RetryClass::Risk))
        .collect();
    assert_eq!(scheduler.transient_retries().len(), 10);
    // Every client went away.
    drop(retries);
    assert_eq!(scheduler.counts().1, 0);
    assert!(scheduler.transient_retries().is_empty());

    // Nothing is scheduled any more, so a newcomer takes no turn behind them
    // and waits as the only request there is.
    let mut late = admitted(&scheduler, &provider, 1).await;
    let (_retry, delay) = blocked(&scheduler, late.pop().unwrap(), RetryClass::Risk);
    assert_eq!(tenths(delay), 66);
}

#[tokio::test(start_paused = true)]
async fn a_released_retry_goes_out_at_once_and_stays_released() {
    let provider = paced(None, 5);
    let scheduler = scheduler(&provider);
    let mut crowd = admitted(&scheduler, &provider, 4).await;
    let (retry, _) = blocked(&scheduler, crowd.pop().unwrap(), RetryClass::Risk);
    retry.release();
    // A longer interval saved afterwards does not take it back.
    let edited = Provider {
        upstream_retry_seconds: 120,
        ..paced(None, 5)
    };
    scheduler.configure(std::slice::from_ref(&edited), true);
    assert_eq!(timed(retry).await, (Ok(()), Duration::ZERO));
}

#[tokio::test(start_paused = true)]
async fn a_reset_sends_the_waiting_retry_and_a_stop_ends_it() {
    let provider = paced(None, 5);
    let scheduler = scheduler(&provider);
    let mut crowd = admitted(&scheduler, &provider, 8).await;

    let (retry, _) = blocked(&scheduler, crowd.pop().unwrap(), RetryClass::Risk);
    let waiting = tokio::spawn(timed(retry));
    tokio::time::sleep(Duration::from_secs(5)).await;
    scheduler.reset_provider(PROVIDER, &[]);
    let (result, waited) = waiting.await.unwrap();
    assert_eq!((result, rounded(waited)), (Ok(()), 5));

    let (retry, _) = blocked(&scheduler, crowd.pop().unwrap(), RetryClass::Risk);
    let waiting = tokio::spawn(timed(retry));
    tokio::time::sleep(Duration::from_secs(5)).await;
    scheduler.configure(std::slice::from_ref(&provider), false);
    assert_eq!(waiting.await.unwrap().0, Err(Rejected::Stopped));
}

#[tokio::test(start_paused = true)]
async fn a_changed_connection_sends_the_waiting_retries_a_gap_apart() {
    // Nine requests press on the provider: 33 s x 14/9 between attempts, and
    // a ninth of that between two retries.
    let retries = |scheduler: &Scheduler, crowd: &mut Vec<Admission>| {
        (0..3)
            .map(|_| {
                let (retry, _) = blocked(scheduler, crowd.pop().unwrap(), RetryClass::Risk);
                tokio::spawn(timed(retry))
            })
            .collect::<Vec<_>>()
    };
    let changed = |provider: &Provider| Provider {
        version: "changed".into(),
        ..provider.clone()
    };
    for (intensity, expected) in [(5, [50, 107, 164]), (0, [50, 50, 50])] {
        let provider = paced(None, intensity);
        let scheduler = scheduler(&provider);
        let mut crowd = admitted(&scheduler, &provider, 9).await;
        let waiting = retries(&scheduler, &mut crowd);
        tokio::time::sleep(Duration::from_secs(5)).await;
        scheduler.configure(&[changed(&provider)], true);
        for (retry, expected) in waiting.into_iter().zip(expected) {
            let (result, waited) = retry.await.unwrap();
            assert_eq!((result, tenths(waited)), (Ok(()), expected), "{intensity}");
        }
    }
    // A manual reset still sends every one of them at once.
    let provider = paced(None, 5);
    let scheduler = scheduler(&provider);
    let mut crowd = admitted(&scheduler, &provider, 9).await;
    let waiting = retries(&scheduler, &mut crowd);
    tokio::time::sleep(Duration::from_secs(5)).await;
    scheduler.reset_provider(PROVIDER, &[]);
    for retry in waiting {
        let (result, waited) = retry.await.unwrap();
        assert_eq!((result, tenths(waited)), (Ok(()), 50));
    }
}

/// An endless provider that rests after a rate limit. Its rate-limit retries
/// are ten seconds apart, so a few requests' share of that ends long before
/// the rest does.
fn resting(rest: Option<u64>, intensity: u8) -> Provider {
    Provider {
        limit_rest_seconds: rest,
        ..paced(Some(10), intensity)
    }
}

fn gate(scheduler: &Scheduler, provider: &Provider) -> Gate {
    scheduler.peek(&[&route(provider.clone())], &Requirement::Resource)[0]
}

fn milliseconds(delay: Duration) -> f64 {
    delay.as_secs_f64() * 1000.0
}

#[tokio::test(start_paused = true)]
async fn a_rate_limit_rests_the_provider_before_anything_is_sent_to_it() {
    // Eight seconds unless the provider says otherwise.
    let provider = resting(None, 5);
    let scheduler = scheduler(&provider);
    let route = route(provider.clone());
    let mut crowd = admitted(&scheduler, &provider, 1).await;
    let (retry, delay) = blocked(&scheduler, crowd.pop().unwrap(), RetryClass::Limit);
    assert_eq!(
        delay,
        Duration::from_secs(8),
        "its own two seconds end inside the rest"
    );
    assert_eq!(
        gate(&scheduler, &provider),
        Gate::Rest(Duration::from_secs(8))
    );

    // A request that cannot wait is told how long; one that can is held.
    let mut budget = Budget::new(30);
    let refused = scheduler
        .acquire_for_immediate(
            std::slice::from_ref(&route),
            false,
            64,
            &mut budget,
            &Requirement::Resource,
            true,
        )
        .await;
    assert_eq!(refused.err(), Some(Rejected::RateLimited(8)));
    let newcomer = tokio::spawn({
        let scheduler = scheduler.clone();
        async move {
            let started = Instant::now();
            let mut budget = Budget::new(30);
            let admission = scheduler
                .acquire(std::slice::from_ref(&route), false, 64, &mut budget)
                .await;
            (admission.is_ok(), started.elapsed())
        }
    });
    let (result, waited) = timed(retry).await;
    assert_eq!((result, rounded(waited)), (Ok(()), 8));
    let (admitted, waited) = newcomer.await.unwrap();
    assert!(admitted);
    assert_eq!(rounded(waited), 8);
    assert_eq!(gate(&scheduler, &provider), Gate::Ready);
}

#[tokio::test(start_paused = true)]
async fn a_rate_limit_during_the_rest_starts_it_over_for_the_whole_line() {
    let provider = resting(None, 5);
    let scheduler = scheduler(&provider);
    // Three are limited together. Each would be back after two ninths of the
    // ten seconds; they go when the rest is over, a third of that apart.
    let own = 10_000.0 * 2.0 / 9.0;
    let mut line: Vec<_> = admitted(&scheduler, &provider, 3)
        .await
        .into_iter()
        .map(|admission| blocked(&scheduler, admission, RetryClass::Limit))
        .collect();
    for (place, (_, delay)) in line.iter().enumerate() {
        let expected = 8_000.0 + own / 3.0 * place as f64;
        assert!(
            (milliseconds(*delay) - expected).abs() < 1.0,
            "place {place} waits {delay:?}"
        );
    }
    let (first, _) = line.remove(0);
    let behind: Vec<_> = line
        .into_iter()
        .map(|(retry, _)| tokio::spawn(timed(retry)))
        .collect();

    // The first goes out as the rest ends and is limited again half a second
    // later, before the next one is due.
    assert_eq!(rounded(timed(first).await.1), 8);
    let mut again = admitted(&scheduler, &provider, 1).await;
    tokio::time::advance(Duration::from_millis(500)).await;
    let (_retry, delay) = blocked(&scheduler, again.pop().unwrap(), RetryClass::Limit);
    assert_eq!(
        gate(&scheduler, &provider),
        Gate::Rest(Duration::from_secs(8))
    );
    // The two that were due wait out the new rest in their old order, and
    // the one just limited takes its turn behind them.
    assert!(
        (milliseconds(delay) - (8_000.0 + own / 3.0 * 2.0)).abs() < 1.0,
        "waits {delay:?}"
    );
    let countdowns: Vec<_> = scheduler
        .transient_retries()
        .into_iter()
        .map(|retry| retry.retry_in)
        .collect();
    assert_eq!(countdowns, [8, 9, 10]);
    for (place, wait) in behind.into_iter().enumerate() {
        let (result, waited) = wait.await.unwrap();
        assert_eq!(result, Ok(()));
        let expected = 16_500.0 + own / 3.0 * place as f64;
        assert!(
            (milliseconds(waited) - expected).abs() < 2.0,
            "place {place} waited {waited:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn the_rest_holds_every_kind_of_retry_and_follows_any_429() {
    let provider = resting(None, 5);
    let scheduler = scheduler(&provider);
    let mut crowd = admitted(&scheduler, &provider, 2).await;
    // A 429 on the risk-control list rests the provider like any other: the
    // retry's own 6.6 seconds become eight.
    let listed = scheduler.retry(&scheduler.capacity_source(PROVIDER), RetryClass::Risk, true);
    drop(crowd.pop());
    assert_eq!(listed.1, Duration::from_secs(8));

    // An ordinary retry would be back three seconds after its failure.
    tokio::time::advance(Duration::from_secs(1)).await;
    let (_ordinary, delay) = blocked(&scheduler, crowd.pop().unwrap(), RetryClass::Normal);
    assert_eq!(delay, Duration::from_secs(7));

    // A failure that is no rate limit does not rest the provider.
    let quiet = resting(None, 5);
    let scheduler = self::scheduler(&quiet);
    let mut crowd = admitted(&scheduler, &quiet, 1).await;
    let (_retry, delay) = blocked(&scheduler, crowd.pop().unwrap(), RetryClass::Risk);
    assert_eq!(tenths(delay), 66);
    assert_eq!(gate(&scheduler, &quiet), Gate::Ready);
}

#[tokio::test(start_paused = true)]
async fn an_edited_rest_moves_the_retry_and_the_gate_at_once() {
    // A second into the default rest the provider is saved with the rest on
    // the left; the lone retry's own wait is two seconds.
    for (edited, expected) in [(Some(20), 20), (Some(3), 3), (Some(0), 2)] {
        let provider = resting(None, 5);
        let scheduler = scheduler(&provider);
        let mut crowd = admitted(&scheduler, &provider, 1).await;
        let (retry, delay) = blocked(&scheduler, crowd.pop().unwrap(), RetryClass::Limit);
        assert_eq!(delay, Duration::from_secs(8));
        let waiting = tokio::spawn(timed(retry));
        tokio::time::sleep(Duration::from_secs(1)).await;
        let edited = resting(edited, 5);
        scheduler.configure(std::slice::from_ref(&edited), true);
        assert_eq!(
            scheduler.transient_retries()[0].retry_in,
            expected - 1,
            "the countdown follows the edit"
        );
        assert_eq!(
            gate(&scheduler, &edited),
            match edited.limit_rest_seconds {
                Some(0) => Gate::Ready,
                rest => Gate::Rest(Duration::from_secs(rest.unwrap() - 1)),
            }
        );
        let (result, waited) = waiting.await.unwrap();
        assert_eq!((result, rounded(waited)), (Ok(()), expected));
    }
}

#[tokio::test(start_paused = true)]
async fn a_provider_set_not_to_rest_is_sent_to_as_before() {
    let provider = resting(Some(0), 5);
    let scheduler = scheduler(&provider);
    let mut crowd = admitted(&scheduler, &provider, 1).await;
    let (_retry, delay) = blocked(&scheduler, crowd.pop().unwrap(), RetryClass::Limit);
    assert_eq!(delay, Duration::from_secs(2));
    assert_eq!(gate(&scheduler, &provider), Gate::Ready);
}

#[tokio::test(start_paused = true)]
async fn a_reset_or_a_restart_ends_the_rest() {
    let provider = resting(None, 5);
    let scheduler = scheduler(&provider);
    let mut crowd = admitted(&scheduler, &provider, 2).await;
    let (retry, _) = blocked(&scheduler, crowd.pop().unwrap(), RetryClass::Limit);
    scheduler.reset_provider(PROVIDER, &[]);
    assert_eq!(gate(&scheduler, &provider), Gate::Ready);
    assert_eq!(timed(retry).await, (Ok(()), Duration::ZERO));

    let (_retry, _) = blocked(&scheduler, crowd.pop().unwrap(), RetryClass::Limit);
    assert_ne!(gate(&scheduler, &provider), Gate::Ready);
    scheduler.configure(std::slice::from_ref(&provider), false);
    scheduler.configure(std::slice::from_ref(&provider), true);
    assert_eq!(gate(&scheduler, &provider), Gate::Ready);
}

#[tokio::test(start_paused = true)]
async fn a_retry_stretches_its_wait_and_its_rest_by_its_own_share() {
    // Half of the fifth a wait may stretch by: a tenth more.
    let stretched = |provider: Provider, class: RetryClass| async move {
        let scheduler = scheduler(&provider);
        scheduler.set_share(Some(0.5));
        let mut crowd = admitted(&scheduler, &provider, 1).await;
        milliseconds(blocked(&scheduler, crowd.pop().unwrap(), class).1)
    };
    for (class, expected) in [
        // 6.6 seconds of the risk-control interval.
        (RetryClass::Risk, 7_260.0),
        // Two seconds of its own, and the eight-second rest.
        (RetryClass::Limit, 8_800.0),
        (RetryClass::Normal, 3_300.0),
    ] {
        let waited = stretched(resting(None, 5), class).await;
        assert!((waited - expected).abs() < 1.0, "{class:?} waits {waited}");
    }
    // Intensity 0 keeps every wait exactly as set.
    for (class, expected) in [
        (RetryClass::Risk, 33_000.0),
        (RetryClass::Limit, 10_000.0),
        (RetryClass::Normal, 3_000.0),
    ] {
        assert_eq!(
            stretched(resting(None, 0), class).await,
            expected,
            "{class:?}"
        );
    }
    let brief = Provider {
        limit_retry_seconds: Some(4),
        ..resting(None, 0)
    };
    assert_eq!(stretched(brief, RetryClass::Limit).await, 8_000.0);

    // A crowd keeps its stretched distance: ten wait 55 seconds and go a
    // tenth of that apart, all of it a tenth longer.
    let provider = paced(None, 5);
    let scheduler = scheduler(&provider);
    scheduler.set_share(Some(0.5));
    let retries: Vec<_> = admitted(&scheduler, &provider, 10)
        .await
        .into_iter()
        .map(|admission| blocked(&scheduler, admission, RetryClass::Risk))
        .collect();
    for (place, (_, delay)) in retries.iter().enumerate() {
        let expected = 60_500.0 + 6_050.0 * place as f64;
        assert!(
            (milliseconds(*delay) - expected).abs() < 1.0,
            "place {place} waits {delay:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn a_random_share_only_ever_lengthens_the_wait() {
    let provider = resting(None, 5);
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..32 {
        let scheduler = scheduler(&provider);
        scheduler.set_share(None);
        let mut crowd = admitted(&scheduler, &provider, 2).await;
        // 6.6 seconds and up to a fifth more.
        let (_risk, delay) = blocked(&scheduler, crowd.pop().unwrap(), RetryClass::Risk);
        assert!(
            delay >= Duration::from_millis(6_600) && delay < Duration::from_millis(7_920),
            "waits {delay:?}"
        );
        seen.insert(delay);
        // Never back before the rest is over.
        let (_limit, delay) = blocked(&scheduler, crowd.pop().unwrap(), RetryClass::Limit);
        assert!(
            delay >= Duration::from_secs(8) && delay < Duration::from_millis(11_520),
            "rests {delay:?}"
        );
        assert!(matches!(gate(&scheduler, &provider), Gate::Rest(left) if left <= delay));
    }
    assert!(seen.len() > 1, "the waits differ");
}

#[tokio::test(start_paused = true)]
async fn a_retry_keeps_its_share_when_the_pace_changes() {
    let provider = paced(None, 5);
    let scheduler = scheduler(&provider);
    scheduler.set_share(None);
    let mut crowd = admitted(&scheduler, &provider, 8).await;
    // Eight in flight: 44 seconds, stretched by the retry's share.
    let (retry, delay) = blocked(&scheduler, crowd.pop().unwrap(), RetryClass::Risk);
    assert!((1.0..1.2).contains(&(delay.as_secs_f64() / 44.0)));
    let waiting = tokio::spawn(timed(retry));
    let doubled = Provider {
        upstream_retry_seconds: 66,
        ..paced(None, 5)
    };
    scheduler.configure(std::slice::from_ref(&doubled), true);
    let (result, waited) = waiting.await.unwrap();
    assert_eq!(result, Ok(()));
    assert!(
        waited.abs_diff(delay * 2) < Duration::from_millis(2),
        "waited {waited:?} after {delay:?}"
    );
}
