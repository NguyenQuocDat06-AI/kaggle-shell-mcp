use super::*;
use crate::worker::{FsEntry, MonitorPayload, TermExecResult};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tokio::sync::mpsc;

    fn ke(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[tokio::test]
    async fn test_arrow_navigation_and_cursor_restore_local_and_remote() {
        let root = std::env::temp_dir().join(format!("kgsh_nav_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let sub_a = root.join("alpha_dir");
        let sub_b = root.join("beta_dir");
        std::fs::create_dir_all(&sub_a).unwrap();
        std::fs::create_dir_all(&sub_b).unwrap();
        std::fs::write(sub_b.join("train.py"), "print('hi')\n").unwrap();
        std::fs::write(root.join(".hidden"), "secret").unwrap();
        std::fs::write(root.join("notes.txt"), "hello").unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let bridge = PythonBridge::discover(None);
        let mut app = App::new(root.clone(), "1".into(), bridge, tx);
        app.left_pane = PaneKind::LocalFiles(LocalPaneState::new(root.clone()));
        app.right_pane = PaneKind::RemoteFiles(RemotePaneState::new("/kaggle/working"));
        app.active_side = Side::Left;
        app.input_focus = InputFocus::None;

        // Hidden dotfile filtered out by default
        if let PaneKind::LocalFiles(ref lp) = app.left_pane {
            assert!(!lp.entries.iter().any(|e| e.name == ".hidden"));
            assert_eq!(lp.entries[0].name, "..");
        }

        // Toggle dotfiles via `.`
        app.handle_key(ke(KeyCode::Char('.'), KeyModifiers::NONE));
        if let PaneKind::LocalFiles(ref lp) = app.left_pane {
            assert!(lp.entries.iter().any(|e| e.name == ".hidden"));
        }
        app.handle_key(ke(KeyCode::Char('.'), KeyModifiers::NONE));

        // Move cursor to `beta_dir` and enter via Right arrow
        let beta_idx = match &app.left_pane {
            PaneKind::LocalFiles(lp) => lp.entries.iter().position(|e| e.name == "beta_dir").unwrap(),
            _ => panic!("expected local"),
        };
        if let PaneKind::LocalFiles(ref mut lp) = app.left_pane {
            lp.cursor = beta_idx;
        }
        app.handle_key(ke(KeyCode::Right, KeyModifiers::NONE));
        if let PaneKind::LocalFiles(ref lp) = app.left_pane {
            assert_eq!(lp.dir, sub_b);
            assert!(lp.entries.iter().any(|e| e.name == "train.py"));
        }

        // Right arrow on file `train.py` opens Preview Drawer; Right arrow again promotes to FileEditor
        let train_idx = match &app.left_pane {
            PaneKind::LocalFiles(lp) => lp.entries.iter().position(|e| e.name == "train.py").unwrap(),
            _ => panic!("expected local"),
        };
        if let PaneKind::LocalFiles(ref mut lp) = app.left_pane {
            lp.cursor = train_idx;
        }
        app.handle_key(ke(KeyCode::Right, KeyModifiers::NONE));
        assert!(app.preview_drawer.is_some());
        app.handle_key(ke(KeyCode::Right, KeyModifiers::NONE));
        assert!(matches!(app.modal, Some(Modal::FileEditor(_))));
        app.modal = None;
        app.preview_drawer = None;

        // Left arrow returns to parent and restores cursor onto `beta_dir`
        app.handle_key(ke(KeyCode::Left, KeyModifiers::NONE));
        if let PaneKind::LocalFiles(ref lp) = app.left_pane {
            assert_eq!(lp.dir, root);
            assert_eq!(lp.entries[lp.cursor].name, "beta_dir");
        }

        // Space toggles selection (skipping `..`) and advances cursor
        if let PaneKind::LocalFiles(ref mut lp) = app.left_pane {
            lp.cursor = 0; // `..`
        }
        app.handle_key(ke(KeyCode::Char(' '), KeyModifiers::NONE));
        if let PaneKind::LocalFiles(ref lp) = app.left_pane {
            assert!(lp.selected.is_empty());
            assert_eq!(lp.cursor, 1);
        }
        app.handle_key(ke(KeyCode::Char(' '), KeyModifiers::NONE));
        if let PaneKind::LocalFiles(ref lp) = app.left_pane {
            assert_eq!(lp.selected.len(), 1);
            assert_eq!(lp.cursor, 2);
        }

        // Shift+A selects all non-`..`, Shift+A again clears all
        app.handle_key(ke(KeyCode::Char('A'), KeyModifiers::SHIFT));
        if let PaneKind::LocalFiles(ref lp) = app.left_pane {
            assert_eq!(lp.selected.len(), 3); // alpha_dir, beta_dir, notes.txt
        }
        app.handle_key(ke(KeyCode::Char('A'), KeyModifiers::SHIFT));
        if let PaneKind::LocalFiles(ref lp) = app.left_pane {
            assert!(lp.selected.is_empty());
        }

        // Copy notes.txt into sub_a via c + navigate_into + p
        let notes_idx = match &app.left_pane {
            PaneKind::LocalFiles(lp) => lp.entries.iter().position(|e| e.name == "notes.txt").unwrap(),
            _ => panic!("expected local"),
        };
        if let PaneKind::LocalFiles(ref mut lp) = app.left_pane {
            lp.cursor = notes_idx;
        }
        app.handle_key(ke(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(app.file_clipboard.is_some());
        if let PaneKind::LocalFiles(ref mut lp) = app.left_pane {
            lp.navigate_into(sub_a.clone());
        }
        app.handle_key(ke(KeyCode::Char('p'), KeyModifiers::NONE));
        assert!(sub_a.join("notes.txt").exists());

        // Live filter `/`
        app.handle_key(ke(KeyCode::Left, KeyModifiers::NONE));
        app.handle_key(ke(KeyCode::Char('/'), KeyModifiers::NONE));
        app.handle_key(ke(KeyCode::Char('n'), KeyModifiers::NONE));
        app.handle_key(ke(KeyCode::Char('o'), KeyModifiers::NONE));
        app.handle_key(ke(KeyCode::Char('t'), KeyModifiers::NONE));
        if let PaneKind::LocalFiles(ref lp) = app.left_pane {
            assert_eq!(lp.entries.iter().filter(|e| e.name != "..").count(), 1);
        }
        app.handle_key(ke(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.input_focus, InputFocus::None);

        // Shift+Right switches focus Left -> Right; Shift+Left switches Right -> Left
        app.handle_key(ke(KeyCode::Right, KeyModifiers::SHIFT));
        assert_eq!(app.active_side, Side::Right);

        // Remote FsListDone + navigate_remote_up cursor restoration
        app.handle_msg(AppMsg::FsListDone {
            side: Side::Right,
            dir: "/kaggle/working/exp1".into(),
            entries: vec![FsEntry {
                name: "ckpt.pt".into(),
                path: "/kaggle/working/exp1/ckpt.pt".into(),
                is_dir: false,
                size_bytes: 1024,
                mtime: 100.0,
            }],
        });
        if let PaneKind::RemoteFiles(ref rp) = app.right_pane {
            assert_eq!(rp.entries[0].name, "..");
            assert_eq!(rp.entries[0].path, "/kaggle/working");
        }
        app.navigate_remote_up(Side::Right);
        if let PaneKind::RemoteFiles(ref rp) = app.right_pane {
            assert_eq!(rp.pending_select_name.as_deref(), Some("exp1"));
        }
        app.handle_msg(AppMsg::FsListDone {
            side: Side::Right,
            dir: "/kaggle/working".into(),
            entries: vec![
                FsEntry { name: "aaa".into(), path: "/kaggle/working/aaa".into(), is_dir: true, size_bytes: 0, mtime: 10.0 },
                FsEntry { name: "exp1".into(), path: "/kaggle/working/exp1".into(), is_dir: true, size_bytes: 0, mtime: 10.0 },
            ],
        });
        if let PaneKind::RemoteFiles(ref rp) = app.right_pane {
            assert_eq!(rp.entries[rp.cursor].name, "exp1");
        }

        app.handle_key(ke(KeyCode::Left, KeyModifiers::SHIFT));
        assert_eq!(app.active_side, Side::Left);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn test_drag_selection_extracts_pane_clamped_text() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir = std::env::temp_dir().join(format!("kgsh_sel_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        let bridge = PythonBridge::discover(None);
        let mut app = App::new(dir.clone(), "1".into(), bridge, tx);
        app.push_activity(ActivityEvent {
            ts: 1.0,
            slot: "1".into(),
            caller: "tui".into(),
            kind: "term_exec".into(),
            summary: "$ python loop.py".into(),
            detail: "epoch 1/10 loss=0.42\nepoch 2/10 loss=0.31".into(),
            exit_code: Some(0),
            duration_sec: Some(1.2),
            seq: None,
        });

        let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();
        terminal
            .draw(|f| {
                crate::ui::render(f, &mut app);
                app.capture_screen_snapshot(f.buffer_mut());
            })
            .unwrap();

        let lr = app.rects.left_rect;
        let ax = lr.x + 2;
        let ay = lr.y + 1;
        let bx = lr.right().saturating_sub(2);
        let by = (lr.y + 4).min(lr.bottom().saturating_sub(3));
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: ax,
            row: ay,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: bx,
            row: by,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: bx,
            row: by,
            modifiers: KeyModifiers::NONE,
        });

        let sel = app.selection.expect("drag selection should be recorded");
        assert!(!sel.dragging);
        let txt = app.extract_selected_text(sel);
        assert!(txt.contains("python loop.py") || txt.contains("epoch"));
        // Clamped strictly inside panel border: no vertical box border at col lr.x
        assert!(!txt.lines().any(|l| l.starts_with('┌') || l.starts_with('└')));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_running_exec_and_pty_live_progress_updates() {
        use crate::worker::PtyLive;

        let dir = std::env::temp_dir().join(format!("kgsh_live_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        let bridge = PythonBridge::discover(None);
        let mut app = App::new(dir.clone(), "1".into(), bridge, tx);

        assert!(!app.has_live_work());
        app.handle_msg(AppMsg::MonitorOk {
            slot: "1".into(),
            rtt_ms: 25.0,
            payload: MonitorPayload {
                status: WorkerStatus::default(),
                jobs: vec![],
                latest_seq: 1,
                events: vec![],
                log_chunk: None,
                pty_live: Some(PtyLive {
                    running: true,
                    session: "default".into(),
                    command: "python3 -u train.py".into(),
                    elapsed_sec: 3.5,
                    output: "step 1/100\nstep 2/100\nstep 3/100".into(),
                }),
                ..Default::default()
            },
        });

        assert!(app.has_live_work());
        let run = app.running_exec.as_ref().expect("running_exec should be active");
        assert_eq!(run.command, "python3 -u train.py");
        assert!(run.live_output.contains("step 3/100"));
        assert!(run.worker_has_live);

        app.handle_msg(AppMsg::ExecDone {
            slot: "1".into(),
            cmd: "python3 -u train.py".into(),
            res: TermExecResult {
                output: "done".into(),
                exit_code: 0,
                duration_sec: 5.0,
                cwd: "/kaggle/working".into(),
            },
        });
        assert!(app.running_exec.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_file_editor_drag_and_shift_selection_copies_clean_code_only() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir = std::env::temp_dir().join(format!("kgsh_ed_sel_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        let bridge = PythonBridge::discover(None);
        let mut app = App::new(dir.clone(), "1".into(), bridge, tx);
        app.modal = Some(Modal::FileEditor(EditorModalState {
            is_local: false,
            is_new: false,
            path: TextField::new("/kaggle/working/script.py"),
            lines: vec![
                "for i in range(0, 10000):".into(),
                "    print(\"hello\")".into(),
            ],
            cur_row: 0,
            cur_col: 0,
            sel_anchor: None,
            scroll_row: 0,
            readonly: false,
            focus_path: false,
            ..Default::default()
        }));

        let mut terminal = Terminal::new(TestBackend::new(120, 32)).unwrap();
        terminal
            .draw(|f| {
                crate::ui::render(f, &mut app);
                app.capture_screen_snapshot(f.buffer_mut());
            })
            .unwrap();

        let body = app.rects.editor_body_rect;
        let code = app.rects.editor_code_rect;
        // Start drag inside line-number gutter at row 0; drag through row 1 end
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: body.x + 1,
            row: body.y,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: code.x + 40,
            row: body.y + 1,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: code.x + 40,
            row: body.y + 1,
            modifiers: KeyModifiers::NONE,
        });

        let selected = match &app.modal {
            Some(Modal::FileEditor(m)) => m.selected_text().expect("editor selection active"),
            _ => panic!("expected editor modal"),
        };
        assert_eq!(selected, "for i in range(0, 10000):\n    print(\"hello\")");
        assert!(!selected.contains("Path"));
        assert!(!selected.contains("1 │"));
        assert!(!selected.contains("2 │"));
        assert!(!selected.contains("/kaggle/working/script.py"));
        assert!(app.selection.is_none());

        // Boundary Shift+Down on last line extends to end of line; Shift+Up on line 0 extends to start
        app.handle_key(ke(KeyCode::Home, KeyModifiers::CONTROL));
        app.handle_key(ke(KeyCode::Down, KeyModifiers::CONTROL | KeyModifiers::SHIFT));
        app.handle_key(ke(KeyCode::Down, KeyModifiers::SHIFT));
        if let Some(Modal::FileEditor(ref m)) = app.modal {
            assert_eq!(m.selected_text().as_deref(), Some("for i in range(0, 10000):\n    print(\"hello\")"));
        }
        app.handle_key(ke(KeyCode::Home, KeyModifiers::CONTROL));
        app.handle_key(ke(KeyCode::Right, KeyModifiers::CONTROL | KeyModifiers::SHIFT));
        if let Some(Modal::FileEditor(ref m)) = app.modal {
            assert_eq!(m.selected_text().as_deref(), Some("for "));
        }
        app.handle_key(ke(KeyCode::Up, KeyModifiers::SHIFT));
        if let Some(Modal::FileEditor(ref m)) = app.modal {
            assert_eq!(m.cur_col, 0);
            assert_eq!(m.selected_text(), None);
        }
        app.handle_key(ke(KeyCode::Right, KeyModifiers::CONTROL | KeyModifiers::SHIFT));
        if let Some(Modal::FileEditor(ref m)) = app.modal {
            assert_eq!(m.selected_text().as_deref(), Some("for "));
        }
        app.handle_key(ke(KeyCode::Char('x'), KeyModifiers::NONE));
        if let Some(Modal::FileEditor(ref m)) = app.modal {
            assert_eq!(m.lines[0], "xi in range(0, 10000):");
        }
        app.handle_key(ke(KeyCode::Char('a'), KeyModifiers::CONTROL));
        if let Some(Modal::FileEditor(ref m)) = app.modal {
            assert_eq!(m.selected_text().as_deref(), Some("xi in range(0, 10000):\n    print(\"hello\")"));
        }
        app.handle_key(ke(KeyCode::Delete, KeyModifiers::NONE));
        if let Some(Modal::FileEditor(ref m)) = app.modal {
            assert_eq!(m.lines, vec![String::new()]);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_text_field_shift_and_ctrl_shift_selection() {
        let mut tf = TextField::new("python train_model.py");
        tf.home_ext(false);
        assert_eq!(tf.selection_range(), None);

        tf.move_right_ext(true);
        tf.move_right_ext(true);
        assert_eq!(tf.selected_text().as_deref(), Some("py"));

        tf.move_word_right_ext(true);
        assert_eq!(tf.selected_text().as_deref(), Some("python "));

        tf.backspace();
        assert_eq!(tf.value, "train_model.py");
        assert_eq!(tf.cursor, 0);

        tf.end_ext(true);
        assert_eq!(tf.selected_text().as_deref(), Some("train_model.py"));
        tf.insert_str("eval.py");
        assert_eq!(tf.value, "eval.py");
        assert_eq!(tf.selection_range(), None);

        tf.select_all();
        assert_eq!(tf.selected_text().as_deref(), Some("eval.py"));
    }

    #[tokio::test]
    async fn test_shell_and_preview_drag_strip_gutters_and_headers() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let dir = std::env::temp_dir().join(format!("kgsh_clean_copy_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        let bridge = PythonBridge::discover(None);
        let mut app = App::new(dir.clone(), "1".into(), bridge, tx);
        app.input_focus = InputFocus::None;
        app.push_activity(ActivityEvent {
            ts: 1.0,
            slot: "1".into(),
            caller: "tui".into(),
            kind: "term_exec".into(),
            summary: "$ echo clean".into(),
            detail: "line_alpha\nline_beta".into(),
            exit_code: Some(0),
            duration_sec: Some(0.1),
            seq: None,
        });
        app.preview_drawer = Some(PreviewDrawerState {
            target_path: "/kaggle/working/demo.py".into(),
            title: "/kaggle/working/demo.py".into(),
            subtitle: "PY · 24 B".into(),
            lines: vec!["x = 1".into(), "print(x)".into()],
            scroll: 0,
        });

        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal
            .draw(|f| {
                crate::ui::render(f, &mut app);
                app.capture_screen_snapshot(f.buffer_mut());
            })
            .unwrap();

        // Shell drag across gutter cols strips `│ ` and `│   `
        let sr = app.rects.shell_content_rect;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: sr.x,
            row: sr.y,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: sr.right().saturating_sub(1),
            row: (sr.y + 2).min(sr.bottom().saturating_sub(1)),
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: sr.right().saturating_sub(1),
            row: (sr.y + 2).min(sr.bottom().saturating_sub(1)),
            modifiers: KeyModifiers::NONE,
        });
        let shell_txt = app.extract_selected_text(app.selection.unwrap());
        assert!(shell_txt.contains("echo clean"));
        assert!(shell_txt.contains("line_alpha"));
        assert!(!shell_txt.lines().any(|l| l.contains('│')));
        assert!(!shell_txt.contains("[NAV]"));

        // Keyboard Shift+Up multiple lines + Ctrl+Shift+Up + Shift+Down in BOTH INPUT & NAV modes
        app.selection = None;
        app.input_focus = InputFocus::ShellPrompt;
        app.handle_key(ke(KeyCode::Up, KeyModifiers::SHIFT));
        let kb_sel1 = app.selection.expect("Shift+Up selects bottom line in ShellPrompt mode");
        assert!(app.extract_selected_text(kb_sel1).contains("line_beta"));

        app.handle_key(ke(KeyCode::Up, KeyModifiers::CONTROL | KeyModifiers::SHIFT));
        let kb_sel2 = app.selection.expect("Ctrl+Shift+Up extends upward cleanly");
        let two_lines = app.extract_selected_text(kb_sel2);
        assert!(two_lines.contains("line_alpha"));
        assert!(two_lines.contains("line_beta"));
        assert!(!two_lines.lines().any(|l| l.contains('│')));

        app.handle_key(ke(KeyCode::Down, KeyModifiers::SHIFT));
        let shrunk = app.extract_selected_text(app.selection.unwrap());
        assert!(!shrunk.contains("line_alpha"));
        assert!(shrunk.contains("line_beta"));

        app.input_focus = InputFocus::None;
        app.selection = None;
        app.handle_key(ke(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(app.active_side, Side::Left);
        let kb_sel = app.selection.expect("Shift+Up creates Shell selection in NAV mode");
        assert!(app.extract_selected_text(kb_sel).contains("line_beta"));

        // Preview drawer drag starting inside gutter strips `   1 │ `
        let pb = app.rects.preview_body_rect;
        let pc = app.rects.preview_code_rect;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pb.x + 1,
            row: pb.y,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: pc.x + 20,
            row: pb.y + 1,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: pc.x + 20,
            row: pb.y + 1,
            modifiers: KeyModifiers::NONE,
        });
        let pv_txt = app.extract_selected_text(app.selection.unwrap());
        assert_eq!(pv_txt, "x = 1\nprint(x)");

        // Preview drawer keyboard Shift+Down selects clean code without gutter
        app.selection = None;
        app.active_side = Side::Right; // non-Shell/Logs/Hub pane falls back to PreviewDrawer when open
        app.handle_key(ke(KeyCode::Down, KeyModifiers::SHIFT));
        app.handle_key(ke(KeyCode::Down, KeyModifiers::CONTROL | KeyModifiers::SHIFT));
        let pv_kb = app.extract_selected_text(app.selection.unwrap());
        assert_eq!(pv_kb, "x = 1\nprint(x)");

        // Plain click on line 1 of PreviewDrawer + Shift+Up selects line 0..1 cleanly
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: pc.x + 2,
            row: pb.y + 1,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: pc.x + 2,
            row: pb.y + 1,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_key(ke(KeyCode::Up, KeyModifiers::SHIFT));
        app.handle_key(ke(KeyCode::Up, KeyModifiers::SHIFT));
        assert_eq!(app.extract_selected_text(app.selection.unwrap()), "x = 1\nprint(x)");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_workers_modal_tab_switches_both_ways() {
        let dir = std::env::temp_dir().join(format!("kgsh_workers_tab_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        let bridge = PythonBridge::discover(None);
        let mut app = App::new(dir.clone(), "1".into(), bridge, tx);

        app.open_workers_modal(0);
        assert!(matches!(app.modal, Some(Modal::Workers(ref m)) if m.tab == 0));

        // Tab toggles 0 -> 1
        app.handle_key(ke(KeyCode::Tab, KeyModifiers::NONE));
        assert!(matches!(app.modal, Some(Modal::Workers(ref m)) if m.tab == 1));

        // Move to middle field (`Datasets (csv)`, idx 4), Tab must still toggle back 1 -> 0
        for _ in 0..4 {
            app.handle_key(ke(KeyCode::Down, KeyModifiers::NONE));
        }
        assert!(matches!(app.modal, Some(Modal::Workers(ref m)) if m.field_idx == 4));
        app.handle_key(ke(KeyCode::Tab, KeyModifiers::NONE));
        assert!(matches!(app.modal, Some(Modal::Workers(ref m)) if m.tab == 0));

        // Right on Tab 0 -> Tab 1; BackTab on Tab 1 -> Tab 0
        app.handle_key(ke(KeyCode::Right, KeyModifiers::NONE));
        assert!(matches!(app.modal, Some(Modal::Workers(ref m)) if m.tab == 1));
        app.handle_key(ke(KeyCode::BackTab, KeyModifiers::SHIFT));
        assert!(matches!(app.modal, Some(Modal::Workers(ref m)) if m.tab == 0));

        // Alt+Right / Alt+Left switch tabs directly
        app.handle_key(ke(KeyCode::Right, KeyModifiers::ALT));
        assert!(matches!(app.modal, Some(Modal::Workers(ref m)) if m.tab == 1));
        app.handle_key(ke(KeyCode::Left, KeyModifiers::ALT));
        assert!(matches!(app.modal, Some(Modal::Workers(ref m)) if m.tab == 0));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_v101_editor_truncation_readonly_dirty_guard_and_save_flow() {
        let dir = std::env::temp_dir().join(format!("kgsh_ed_v101_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        let bridge = PythonBridge::discover(None);
        let mut app = App::new(dir.clone(), "1".into(), bridge, tx);

        // 1. Truncated remote file MUST open strictly READ-ONLY to prevent 64KB+ tail truncation!
        app.handle_msg(AppMsg::FsReadDone {
            path: "/kaggle/working/large_train.py".into(),
            content: "import torch\n".into(),
            truncated: true,
            is_binary: false,
            size_bytes: 500_000,
        });
        match &app.modal {
            Some(Modal::FileEditor(m)) => {
                assert!(m.readonly);
                assert!(m.readonly_reason.contains("truncated"));
            }
            _ => panic!("expected FileEditor modal"),
        }
        // Typing in readonly editor must not mutate lines or set dirty
        app.handle_key(ke(KeyCode::Char('Z'), KeyModifiers::NONE));
        if let Some(Modal::FileEditor(ref m)) = app.modal {
            assert_eq!(m.lines[0], "import torch");
            assert!(!m.dirty);
        }
        // Esc closes clean readonly editor immediately
        app.handle_key(ke(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.modal.is_none());

        // 2. Editable local file: dirty Esc requires confirmation; Ctrl+S keeps editor open until FsSaveDone!
        let local_py = dir.join("model.py");
        std::fs::write(&local_py, "x = 1\n").unwrap();
        app.open_local_file_editor(local_py);
        assert!(matches!(app.modal, Some(Modal::FileEditor(ref m)) if !m.readonly && !m.dirty));

        app.handle_key(ke(KeyCode::Char('#'), KeyModifiers::NONE));
        assert!(matches!(app.modal, Some(Modal::FileEditor(ref m)) if m.dirty && !m.confirm_discard));

        // First Esc warns instead of discarding unsaved edits
        app.handle_key(ke(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.modal, Some(Modal::FileEditor(ref m)) if m.confirm_discard));
        // Second Esc confirms discard
        app.handle_key(ke(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.modal.is_none());

        // Re-open remote non-truncated file, edit, then FsSaveDone(ok=true) closes modal cleanly
        app.handle_msg(AppMsg::FsReadDone {
            path: "/kaggle/working/small.py".into(),
            content: "lr = 1e-4\n".into(),
            truncated: false,
            is_binary: false,
            size_bytes: 10,
        });
        app.handle_key(ke(KeyCode::End, KeyModifiers::NONE));
        app.handle_key(ke(KeyCode::Enter, KeyModifiers::NONE));
        app.handle_key(ke(KeyCode::Tab, KeyModifiers::NONE));
        if let Some(Modal::FileEditor(ref m)) = app.modal {
            assert_eq!(m.lines[1], "    ");
            assert!(m.dirty);
        }
        // Failed save keeps modal open with dirty=true!
        app.handle_msg(AppMsg::FsSaveDone {
            path: "/kaggle/working/small.py".into(),
            ok: false,
            err: "HTTP 524".into(),
        });
        assert!(matches!(app.modal, Some(Modal::FileEditor(ref m)) if m.dirty && !m.saving));
        // Successful save clears dirty/saving while keeping editor open; single Esc then closes cleanly
        app.handle_msg(AppMsg::FsSaveDone {
            path: "/kaggle/working/small.py".into(),
            ok: true,
            err: String::new(),
        });
        assert!(matches!(app.modal, Some(Modal::FileEditor(ref m)) if !m.dirty && !m.saving));
        app.handle_key(ke(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.modal.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_v101_yank_buffer_osc52_and_bracketed_paste_sanitization() {
        let dir = std::env::temp_dir().join(format!("kgsh_clip_v101_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        let bridge = PythonBridge::discover(None);
        let mut app = App::new(dir.clone(), "1".into(), bridge, tx);

        // OSC 52 sequence validation
        assert_eq!(
            build_osc52_sequence("hello"),
            Some("\x1b]52;c;aGVsbG8=\x07".to_string())
        );
        assert_eq!(build_osc52_sequence(""), None);
        assert_eq!(build_osc52_sequence(&"A".repeat(60_000)), None);

        // Ctrl+X + Ctrl+V in ShellPrompt preserves cut text via internal yank_buffer on headless SSH!
        app.input_focus = InputFocus::ShellPrompt;
        app.prompt = TextField::new("torchrun train.py");
        app.prompt.select_all();
        app.handle_key(ke(KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert_eq!(app.prompt.value, "");
        assert_eq!(app.yank_buffer, "torchrun train.py");

        app.handle_key(ke(KeyCode::Char('v'), KeyModifiers::CONTROL));
        assert_eq!(app.prompt.value, "torchrun train.py");

        // Bracketed paste strips ESC / C0 terminal injection bytes
        app.prompt = TextField::new("");
        app.handle_paste("echo safe\x1b[2J\x07\r\necho line2".into());
        assert!(!app.prompt.value.contains('\x1b'));
        assert!(!app.prompt.value.contains('\x07'));
        assert!(app.prompt.value.contains("echo safe"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_v101_poll_in_flight_and_tui_session_isolation() {
        use crate::worker::PtyLive;
        use std::collections::BTreeMap;

        let dir = std::env::temp_dir().join(format!("kgsh_iso_v101_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let (tx, _rx) = mpsc::unbounded_channel();
        let bridge = PythonBridge::discover(None);
        let mut app = App::new(dir.clone(), "1".into(), bridge, tx);

        // Agent PTY running in `pty_sessions_live["default"]` must NOT hijack TUI's `running_exec`!
        let mut live_map = BTreeMap::new();
        live_map.insert(
            "default".to_string(),
            PtyLive {
                running: true,
                session: "default".into(),
                command: "agent_secret_scan.py".into(),
                elapsed_sec: 2.0,
                output: "agent out".into(),
            },
        );
        app.handle_msg(AppMsg::MonitorOk {
            slot: "1".into(),
            rtt_ms: 12.0,
            payload: MonitorPayload {
                pty_sessions_live: live_map,
                ..Default::default()
            },
        });
        assert!(app.running_exec.is_none());

        // Batch poll respects `poll_backoff_until` before spawning Python subprocess
        app.sessions.insert(
            "batch-1".into(),
            ActiveSession {
                slot: "batch-1".into(),
                mode: "batch".into(),
                kernel_ref: "alice/kgsh-batch-1-abc123".into(),
                ..Default::default()
            },
        );
        app.current_slot = "batch-1".into();
        app.poll_backoff_until = Some(Instant::now() + Duration::from_secs(30));
        app.poll_tick();
        assert!(app.poll_in_flight.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }


