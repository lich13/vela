use super::*;
use crate::usage::antigravity::quota::Period;

fn account(id: &str, title: Title, lines: Vec<Line>, note: Option<&str>) -> Account {
    Account {
        id: id.into(),
        title,
        lines,
        note: note.map(str::to_owned),
    }
}
/// A title that says no more than whose the account is.
fn titled(client: &'static str) -> Title {
    Title {
        client,
        ..Title::default()
    }
}
fn periods(name: Option<&'static str>, periods: &[(&str, Option<f64>)]) -> Line {
    Line {
        name,
        periods: periods
            .iter()
            .map(|(label, percent)| ((*label).to_owned(), *percent))
            .collect(),
    }
}
fn texts(rows: &[Row]) -> Vec<(&str, bool)> {
    rows.iter()
        .map(|row| (row.text.as_str(), row.enabled))
        .collect()
}
fn styles(rows: &[Row]) -> Vec<&Style> {
    rows.iter().map(|row| &row.style).collect()
}
fn gauge(label: &str, left: Option<(u8, usize, Tone)>) -> Gauge {
    Gauge {
        label: label.into(),
        left: left.map(|(percent, filled, tone)| Left {
            percent,
            filled,
            tone,
        }),
    }
}
/// Accounts of every kind the menu lists: read, partly read with a failure
/// beside it, not read yet, and with no period at all.
fn several() -> Vec<Row> {
    rows(&[
        account(
            "work",
            title(ClientId::Codex, Some("work"), Some("Plus"), false),
            vec![periods(None, &[("5h", Some(72.0)), ("7d", Some(31.0))])],
            None,
        ),
        account(
            "agy",
            title(ClientId::Agy, Some("work"), None, true),
            vec![
                periods(Some("Gemini"), &[("5h", Some(82.0)), ("7d", None)]),
                periods(Some("Claude / GPT"), &[]),
            ],
            Some("Antigravity 额度查询受限，请稍后重试"),
        ),
        account(
            "new",
            title(ClientId::Codex, Some("new"), None, false),
            Vec::new(),
            Some(READING),
        ),
        account(
            "bare",
            title(ClientId::Grok, None, None, false),
            vec![periods(None, &[])],
            None,
        ),
    ])
}

#[test]
fn a_bar_is_empty_only_where_it_reads_0_and_full_only_where_it_reads_100() {
    let text = |percent: f64| Left::of(percent).text();
    assert_eq!(text(0.0), "\u{2007}\u{2007}0% ▱▱▱▱▱▱▱▱");
    // The bar goes by the number, which is a whole percentage.
    assert_eq!(text(0.4), "\u{2007}\u{2007}0% ▱▱▱▱▱▱▱▱");
    // Too little for a cell, and a cell all the same: the number is not 0%.
    assert_eq!(text(0.5), "\u{2007}\u{2007}1% ▰▱▱▱▱▱▱▱");
    assert_eq!(text(1.0), "\u{2007}\u{2007}1% ▰▱▱▱▱▱▱▱");
    assert_eq!(text(50.0), "\u{2007}50% ▰▰▰▰▱▱▱▱");
    assert_eq!(text(72.4), "\u{2007}72% ▰▰▰▰▰▰▱▱");
    assert_eq!(text(99.0), "\u{2007}99% ▰▰▰▰▰▰▰▱");
    assert_eq!(text(99.4), "\u{2007}99% ▰▰▰▰▰▰▰▱");
    assert_eq!(text(99.6), "100% ▰▰▰▰▰▰▰▰");
    // A service that counts past its own limit is not shown past the ends.
    assert_eq!(Left::of(-3.0), Left::of(0.0));
    assert_eq!(Left::of(140.0), Left::of(100.0));
}

#[test]
fn the_bar_that_is_drawn_has_twelve_cells_and_goes_by_the_number_as_well() {
    let filled = |percent: f64| Left::of(percent).filled;
    assert_eq!(filled(0.0), 0);
    assert_eq!(filled(0.4), 0);
    // It reads 1%: a cell, though that is far less than a twelfth.
    assert_eq!(filled(0.5), 1);
    assert_eq!(filled(4.0), 1);
    // A cell is a twelfth, and the nearest count of them is taken.
    assert_eq!(filled(12.0), 1);
    assert_eq!(filled(13.0), 2);
    assert_eq!(filled(50.0), 6);
    assert_eq!(filled(72.4), 9);
    // Nearer twelve cells than eleven, and eleven all the same: the number
    // is not 100%.
    assert_eq!(filled(96.0), 11);
    assert_eq!(filled(99.4), 11);
    assert_eq!(filled(99.6), 12);
    assert_eq!(filled(140.0), 12);
}

#[test]
fn a_bar_is_red_with_a_tenth_left_or_less_and_orange_with_a_fifth_or_less() {
    let tone = |percent: f64| Left::of(percent).tone;
    assert_eq!(tone(0.0), Tone::Red);
    assert_eq!(tone(10.0), Tone::Red);
    // It goes by the number as it reads: 10.4 reads 10%, and 10.5 reads 11%.
    assert_eq!(tone(10.4), Tone::Red);
    assert_eq!(tone(10.5), Tone::Orange);
    assert_eq!(tone(11.0), Tone::Orange);
    assert_eq!(tone(20.0), Tone::Orange);
    assert_eq!(tone(20.4), Tone::Orange);
    assert_eq!(tone(20.5), Tone::Normal);
    assert_eq!(tone(21.0), Tone::Normal);
    assert_eq!(tone(100.0), Tone::Normal);
}

#[test]
fn every_account_has_a_title_that_cannot_be_chosen_and_rows_under_it_that_can() {
    assert_eq!(
        texts(&several()),
        [
            ("Codex · work（Plus）", false),
            // What a period is called, the number, the bar.
            (
                "    5h \u{2007}72% ▰▰▰▰▰▰▱▱   7d \u{2007}31% ▰▰▱▱▱▱▱▱",
                true
            ),
            ("Antigravity · work ✓", false),
            // A row without a period is left out; one whose service gave no
            // number says so.
            ("    5h \u{2007}82% ▰▰▰▰▰▰▰▱   7d —   Gemini", true),
            ("    Antigravity 额度查询受限，请稍后重试", true),
            ("Codex · new", false),
            ("    读取中…", true),
            ("Grok Build", false),
            ("    —", true),
        ]
    );
    assert_eq!(texts(&rows(&[])), [(NOBODY, false)]);
}

#[test]
fn a_row_is_drawn_as_the_title_or_the_periods_it_is_and_whatever_else_as_its_text() {
    assert_eq!(
        styles(&several()),
        [
            &Style::Title(title(ClientId::Codex, Some("work"), Some("Plus"), false)),
            &Style::Detail {
                name: None,
                periods: vec![
                    gauge("5h", Some((72, 9, Tone::Normal))),
                    gauge("7d", Some((31, 4, Tone::Normal))),
                ],
            },
            &Style::Title(title(ClientId::Agy, Some("work"), None, true)),
            // A period without a number has neither a bar nor a tone.
            &Style::Detail {
                name: Some("Gemini"),
                periods: vec![gauge("5h", Some((82, 10, Tone::Normal))), gauge("7d", None)],
            },
            // Why the account could not be read, that it is being read, that
            // it has no period.
            &Style::Plain,
            &Style::Title(title(ClientId::Codex, Some("new"), None, false)),
            &Style::Plain,
            &Style::Title(title(ClientId::Grok, None, None, false)),
            &Style::Plain,
        ]
    );
    assert_eq!(styles(&rows(&[])), [&Style::Plain]);
}

#[test]
fn a_row_that_turns_from_periods_into_a_failure_is_drawn_as_its_text() {
    // The second row of the menu stays the same item whatever it comes to
    // say, and what the item was drawn as before must not stay with it.
    let second = |account: &Account| {
        let mut rows = rows(std::slice::from_ref(account));
        assert_eq!(rows.len(), 2);
        rows.remove(1)
    };
    let mut account = listed(&[], "a".into(), titled("A"));
    assert_eq!(second(&account).text, "    读取中…");
    assert_eq!(second(&account).style, Style::Plain);

    account.lines = vec![periods(None, &[("5h", Some(15.0))])];
    account.note = None;
    assert_eq!(
        second(&account).style,
        Style::Detail {
            name: None,
            periods: vec![gauge("5h", Some((15, 2, Tone::Orange)))],
        }
    );

    account.failed(codex_quota::OVER.into(), true);
    assert_eq!(second(&account).text, "    登录已过期，请重新登录");
    assert_eq!(second(&account).style, Style::Plain);

    account.lines = vec![periods(None, &[("5h", Some(9.0))])];
    account.note = None;
    assert_eq!(
        second(&account).style,
        Style::Detail {
            name: None,
            periods: vec![gauge("5h", Some((9, 1, Tone::Red)))],
        }
    );
}

#[test]
fn the_menu_is_told_how_each_row_is_drawn_as_json() {
    let rows = rows(&[account(
        "agy",
        title(ClientId::Agy, Some("work"), Some("Pro"), true),
        vec![periods(Some("Gemini"), &[("5h", Some(8.0)), ("7d", None)])],
        Some(READING),
    )]);
    assert_eq!(
        serde_json::to_value(styles(&rows)).unwrap(),
        serde_json::json!([
            {
                "kind": "title",
                "client": "Antigravity",
                "name": "work",
                "plan": "Pro",
                "current": true,
            },
            {
                "kind": "detail",
                "name": "Gemini",
                "periods": [
                    {"label": "5h", "left": {"percent": 8, "filled": 1, "tone": "red"}},
                    {"label": "7d", "left": null},
                ],
            },
            {"kind": "plain"},
        ])
    );
    // What a title or a row of periods lacks is told as lacking.
    assert_eq!(
        serde_json::to_value(Style::Title(titled("Grok Build"))).unwrap(),
        serde_json::json!({
            "kind": "title",
            "client": "Grok Build",
            "name": null,
            "plan": null,
            "current": false,
        })
    );
    assert_eq!(
        serde_json::to_value(Style::Detail {
            name: None,
            periods: vec![gauge("周", Some((20, 2, Tone::Orange)))],
        })
        .unwrap(),
        serde_json::json!({
            "kind": "detail",
            "name": null,
            "periods": [
                {"label": "周", "left": {"percent": 20, "filled": 2, "tone": "orange"}},
            ],
        })
    );
    assert_eq!(
        serde_json::to_value(Left::of(21.0)).unwrap(),
        serde_json::json!({"percent": 21, "filled": 3, "tone": "normal"})
    );
}

#[test]
fn as_many_rows_of_other_accounts_differ_in_which_of_them_can_be_chosen() {
    // Two accounts of two rows each and three of one: six rows either way.
    let line = || vec![periods(None, &[("5h", Some(50.0))])];
    let two = rows(&[
        account("a", titled("A"), [line(), line()].concat(), None),
        account("b", titled("B"), [line(), line()].concat(), None),
    ]);
    let three = rows(&[
        account("a", titled("A"), line(), None),
        account("b", titled("B"), line(), None),
        account("c", titled("C"), line(), None),
    ]);
    assert_eq!(two.len(), three.len());
    let chosen = |rows: &[Row]| rows.iter().map(|row| row.enabled).collect::<Vec<_>>();
    assert_eq!(chosen(&two), [false, true, true, false, true, true]);
    assert_eq!(chosen(&three), [false, true, false, true, false, true]);
}

#[test]
fn a_title_names_the_client_the_account_its_plan_and_the_one_signed_in_to() {
    assert_eq!(
        title(ClientId::Codex, Some("work"), Some("Plus"), true).text(),
        "Codex · work（Plus） ✓"
    );
    assert_eq!(
        title(ClientId::Claude, Some("f***@example.invalid"), None, false).text(),
        "Claude Code · f***@example.invalid"
    );
    assert_eq!(
        title(ClientId::Grok, None, None, false).text(),
        "Grok Build"
    );
    assert_eq!(capitalized("max"), "Max");
    assert_eq!(capitalized(""), "");
}

#[test]
fn an_antigravity_account_shows_the_quotas_it_has() {
    let period = |percent: Option<f64>, without: Option<Without>| Period {
        start: 0,
        end: 1,
        remaining_percent: percent,
        without,
    };
    let lines = antigravity_lines(&Periods::from([
        ("gemini-5h".to_owned(), period(Some(82.0), None)),
        // Untouched: it has no reset yet, and all of it is left.
        (
            "gemini-weekly".to_owned(),
            period(Some(100.0), Some(Without::NoReset)),
        ),
        ("3p-5h".to_owned(), period(None, Some(Without::Disabled))),
        ("3p-weekly".to_owned(), period(None, None)),
    ]));
    assert_eq!(
        lines,
        [
            periods(Some("Gemini"), &[("5h", Some(82.0)), ("7d", Some(100.0))]),
            periods(Some("Claude / GPT"), &[("7d", None)]),
        ]
    );
    // Nothing saved: neither row has a period, and the menu shows neither.
    assert!(antigravity_lines(&Periods::new())
        .iter()
        .all(|line| line.periods.is_empty()));
}

#[test]
fn an_account_listed_again_keeps_what_was_read_of_it_until_it_is_read_again() {
    let earlier = [
        account(
            "read",
            titled("old title"),
            vec![periods(None, &[("5h", Some(40.0))])],
            Some("查询受限，请稍后重试"),
        ),
        account(
            "failed",
            titled("old title"),
            Vec::new(),
            Some("登录已过期，请重新登录"),
        ),
    ];
    let read = listed(&earlier, "read".into(), titled("new title"));
    assert_eq!(read.title, titled("new title"));
    assert_eq!(read.lines, earlier[0].lines);
    // Why it could not be read last time stands beside what was read before.
    assert_eq!(read.note.as_deref(), Some("查询受限，请稍后重试"));
    // With nothing to show, the account says that it is being read.
    for id in ["failed", "new"] {
        let account = listed(&earlier, id.into(), titled("title"));
        assert!(account.lines.is_empty());
        assert_eq!(account.note.as_deref(), Some(READING));
    }
}

#[test]
fn a_failure_is_said_under_what_was_read_before_and_a_login_that_is_over_shows_only_that() {
    let read = || {
        account(
            "a",
            titled("A"),
            vec![periods(None, &[("5h", Some(40.0))])],
            None,
        )
    };
    let mut limited = read();
    limited.failed("查询受限，请稍后重试".into(), false);
    assert_eq!(limited.lines, read().lines);
    assert_eq!(limited.note.as_deref(), Some("查询受限，请稍后重试"));

    let mut over = read();
    over.failed(codex_quota::OVER.into(), true);
    assert_eq!(
        texts(&rows(&[over])),
        [("A", false), ("    登录已过期，请重新登录", true)]
    );
}

fn grok(
    login: &str,
    week: Option<f64>,
    month: Option<f64>,
    failed: Option<&str>,
) -> grok_quota::Quota {
    grok_quota::Quota {
        login: login.into(),
        week,
        month,
        failed: failed.map(str::to_owned),
    }
}

#[test]
fn a_grok_period_that_could_not_be_asked_for_stays_as_it_was_read_before() {
    assert_eq!(grok_account(None, None), None);
    let read = grok_account(None, Some(grok("a", Some(66.0), Some(88.0), None))).unwrap();
    assert_eq!(read.id, "a");
    assert_eq!(read.title, title(ClientId::Grok, None, None, false));
    let both = [periods(None, &[("周", Some(66.0)), ("月", Some(88.0))])];
    assert_eq!(
        (read.lines.as_slice(), read.note.as_deref()),
        (&both[..], None)
    );

    // The month's question failed: the week is the one just told, the month
    // the one read before, and why is said under them.
    let partly = grok("a", Some(50.0), None, Some("额度服务没有应答"));
    let partly = grok_account(Some(read.clone()), Some(partly)).unwrap();
    assert_eq!(
        partly.lines,
        [periods(None, &[("周", Some(50.0)), ("月", Some(88.0))])]
    );
    assert_eq!(partly.note.as_deref(), Some("额度服务没有应答"));
    // Neither was answered: everything stays.
    let limited = grok("a", None, None, Some("查询受限，请稍后重试"));
    let limited = grok_account(Some(partly.clone()), Some(limited)).unwrap();
    assert_eq!(limited.lines, partly.lines);
    assert_eq!(limited.note.as_deref(), Some("查询受限，请稍后重试"));

    // Both answered and told no month: the plan has none any longer.
    let weekly = grok_account(Some(limited), Some(grok("a", Some(40.0), None, None))).unwrap();
    assert_eq!(weekly.lines, [periods(None, &[("周", Some(40.0))])]);
    assert_eq!(weekly.note, None);
    // Both answered and told nothing: there is no period to show.
    let bare = grok_account(Some(weekly.clone()), Some(grok("a", None, None, None))).unwrap();
    assert_eq!(
        texts(&rows(&[bare])),
        [("Grok Build", false), ("    —", true)]
    );

    // A login that is over shows only that.
    let over = grok("a", None, None, Some(grok_quota::OVER));
    let over = grok_account(Some(weekly.clone()), Some(over)).unwrap();
    assert!(over.lines.is_empty());
    assert_eq!(over.note.as_deref(), Some(grok_quota::OVER));
    // Signed out, the account is gone.
    assert_eq!(grok_account(Some(weekly), None), None);
}

#[test]
fn what_was_read_with_another_grok_login_is_not_shown_under_this_one() {
    let read = grok_account(None, Some(grok("a", Some(66.0), Some(88.0), None)));
    // Another account signed in, and could not be read.
    let failed = grok("b", None, None, Some("额度服务没有应答"));
    let other = grok_account(read.clone(), Some(failed)).unwrap();
    assert_eq!(other.id, "b");
    assert!(other.lines.is_empty());
    assert_eq!(
        texts(&rows(&[other])),
        [("Grok Build", false), ("    额度服务没有应答", true)]
    );
    // Nor is a period of the other account kept beside one of this one.
    let partly = grok("b", Some(10.0), None, Some("额度服务暂不可用"));
    let other = grok_account(read, Some(partly)).unwrap();
    assert_eq!(other.lines, [periods(None, &[("周", Some(10.0))])]);
}

#[test]
fn a_menu_opened_while_the_accounts_are_only_being_listed_is_asked_for_once_they_are() {
    use Reading::*;
    // Nothing under way: a reading is started.
    assert_eq!(Idle.begin(false), (Listing, true));
    assert_eq!(Idle.begin(true), (Asking, true));
    // The listing at the start is under way when the menu first opens: no
    // second reading is started, and the one under way goes on to ask.
    assert_eq!(Listing.begin(true), (ListingThenAsking, false));
    assert_eq!(ListingThenAsking.end(), (Asking, true));
    assert_eq!(Asking.end(), (Idle, false));
    // Whatever else comes while one is under way is left to it.
    assert_eq!(Listing.begin(false), (Listing, false));
    for under_way in [ListingThenAsking, Asking] {
        for ask in [false, true] {
            assert_eq!(under_way.begin(ask), (under_way, false));
        }
    }
    // A listing nobody asked more of ends there.
    assert_eq!(Listing.end(), (Idle, false));
    assert_eq!(Idle.end(), (Idle, false));
}
