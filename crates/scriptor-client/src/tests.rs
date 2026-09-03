use super::*;

/// A `.docx` built from paragraph texts - a comparison fixture.
fn docx(paras: &[&str]) -> Vec<u8> {
    let d = CollabDoc::new();
    for p in paras {
        d.append_paragraph(&[Run::plain(*p)], None).unwrap();
    }
    d.to_docx_bytes().unwrap()
}

/// An agent compares its document against a revised version and gets a redline (attributed to
/// itself) plus the change manifest - the blacklining path.
#[test]
fn agent_compare_produces_an_attributed_redline_and_manifest() -> Result<()> {
    let original = docx(&["The term is five years.", "Signatures."]);
    let revised = docx(&["The term is ten years.", "Signatures."]);

    let agent = AgentPeer::from_docx_bytes("AI Agent (legal-bot)", &original)?;
    let result = agent.compare_with(&revised)?;

    assert!(!result.redline.is_empty());
    assert!(!result.manifest.changes.is_empty(), "the term change should be redlined");

    // Every emitted revision is attributed to the agent.
    let red = CollabDoc::from_docx_bytes(&result.redline)?;
    let attributed = red
        .paragraphs()?
        .iter()
        .flat_map(|p| p.runs.clone())
        .any(|r| r.track.as_ref().is_some_and(|t| t.author == "AI Agent (legal-bot)"));
    assert!(attributed, "redline revisions must carry the agent's author");
    Ok(())
}

/// The wire form of a comparison is JSON-serializable and round-trips (the agent's reasoning
/// surface for an out-of-process caller).
#[test]
fn compare_wire_dto_round_trips_through_json() -> Result<()> {
    let original = docx(&["Clause A original.", "Tail."]);
    let revised = docx(&["Clause A revised.", "Tail."]);

    let agent = AgentPeer::from_docx_bytes("Compare Bot", &original)?;
    let dto = agent.compare_with_dto(&revised)?;
    assert!(!dto.changes.is_empty());
    assert!(dto.changes.iter().all(|c| !c.kind.is_empty()));

    let json = serde_json::to_string(&dto).unwrap();
    assert!(json.contains("\"summary\""));
    let back: wire::CompareResultDto = serde_json::from_str(&json).unwrap();
    assert_eq!(back, dto);
    Ok(())
}

/// The standalone `compare_docx` (two arbitrary documents, no live peer) attributes the redline
/// to the given author.
#[test]
fn compare_docx_free_function_attributes_the_author() -> Result<()> {
    let a = docx(&["Old provision here.", "Keep."]);
    let b = docx(&["Keep.", "Old provision here."]); // a relocation -> a move
    let result = compare_docx(&a, &b, "Reviewer X")?;
    assert!(!result.manifest.changes.is_empty());
    let red = CollabDoc::from_docx_bytes(&result.redline)?;
    let attributed = red
        .paragraphs()?
        .iter()
        .flat_map(|p| p.runs.clone())
        .any(|r| r.track.as_ref().is_some_and(|t| t.author == "Reviewer X"));
    assert!(attributed);
    Ok(())
}

/// The semantic overlay annotates a comparison (materiality / risk per change) and enforces the
/// trust boundary: a citation to a non-existent change is rejected, so the overlay describes the
/// redline but can never invent or alter it.
#[test]
fn semantic_overlay_annotates_and_enforces_the_trust_boundary() -> Result<()> {
    let original = docx(&["The Supplier shall indemnify the Buyer.", "Tail."]);
    let revised = docx(&["The Supplier may indemnify the Buyer.", "Tail."]);
    let agent = AgentPeer::from_docx_bytes("legal-bot", &original)?;
    let result = agent.compare_with(&revised)?;
    assert!(!result.manifest.changes.is_empty());

    // The integrator's LLM flags change #0 as substantive with a risk.
    let anns = vec![Annotation {
        change: 0,
        materiality: Materiality::Substantive,
        category: "obligation".into(),
        summary: "Weakens the indemnity from mandatory to permissive.".into(),
        risks: vec!["'shall' -> 'may' weakens the obligation".into()],
    }];
    let dto = annotate_comparison(result.manifest.clone(), anns)?;
    assert!(dto.changes.iter().any(|c| c.materiality.as_deref() == Some("substantive")));
    assert!(dto.changes.iter().any(|c| !c.risks.is_empty()));
    let json = serde_json::to_string(&dto).unwrap();
    let back: wire::AnnotatedCompareResultDto = serde_json::from_str(&json).unwrap();
    assert_eq!(back, dto);

    // A hallucinated citation is refused.
    let bad = vec![Annotation {
        change: 999,
        materiality: Materiality::Trivial,
        category: String::new(),
        summary: String::new(),
        risks: vec![],
    }];
    assert!(annotate_comparison(result.manifest, bad).is_err());
    Ok(())
}

/// An agent joins a document, proposes an insertion, and the change lands on a human peer as a
/// tracked insertion attributed to the agent - with the original text intact.
#[test]
fn agent_insertion_is_an_attributed_tracked_change() -> Result<()> {
    // A human peer starts with one paragraph.
    let human = CollabDoc::new();
    human.append_paragraph(&[Run::plain("The cat sat.")], None)?;

    // The agent joins from the human's snapshot and proposes inserting "quick " before "cat".
    let agent = AgentPeer::join("AI Agent (editor)", &human.snapshot()?)?;
    let id =
        agent.propose_insertion(0, 4, "quick ", "2026-06-17T12:00:00Z", "tighten phrasing")?;
    assert_eq!(id, 1, "first revision id should be 1");

    // The human merges the agent's update.
    human.merge(&agent.export()?)?;

    let runs = &human.paragraphs()?[0].runs;
    // "The " | "quick "(ins) | "cat sat."
    let inserted = runs.iter().find(|r| r.track.is_some()).expect("no tracked insertion landed");
    assert_eq!(inserted.text, "quick ");
    let track = inserted.track.as_ref().unwrap();
    assert_eq!(track.author, "AI Agent (editor)");
    assert_eq!(track.id, 1);

    // Original text is all still present (nothing destroyed).
    let full: String = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(full, "The quick cat sat.");
    Ok(())
}

/// A proposed deletion marks the text rather than removing it (so it can be rejected), and is
/// attributed to the agent.
#[test]
fn agent_deletion_retains_text_for_rejection() -> Result<()> {
    let human = CollabDoc::new();
    human.append_paragraph(&[Run::plain("Delete me please.")], None)?;

    let agent = AgentPeer::join("AI Agent (editor)", &human.snapshot()?)?;
    agent.propose_deletion(0, 0..7, "2026-06-17T12:00:00Z", "redundant")?; // "Delete "
    human.merge(&agent.export()?)?;

    let runs = &human.paragraphs()?[0].runs;
    let deleted = runs.iter().find(|r| r.track.is_some()).expect("no tracked deletion");
    assert_eq!(deleted.text, "Delete ");
    assert_eq!(deleted.track.as_ref().unwrap().author, "AI Agent (editor)");
    // Text retained: the full string is unchanged until the suggestion is accepted.
    let full: String = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(full, "Delete me please.");
    Ok(())
}

/// Two agents proposing concurrently still converge on the text (CRDT merge); this also
/// exercises the same-position concurrent-insert case.
#[test]
fn concurrent_agents_converge_on_text() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("Shared line.")], None)?;
    let snap = base.snapshot()?;

    let a = AgentPeer::join("Agent A", &snap)?;
    let b = AgentPeer::join("Agent B", &snap)?;
    a.propose_insertion(0, 12, " A-note.", "2026-06-17T12:00:00Z", "a")?;
    b.propose_insertion(0, 12, " B-note.", "2026-06-17T12:00:00Z", "b")?;

    // Exchange and converge.
    let (ea, eb) = (a.export()?, b.export()?);
    a.merge(&eb)?;
    b.merge(&ea)?;
    assert_eq!(a.paragraphs()?, b.paragraphs()?, "agents did not converge");
    let full: String = a.paragraphs()?[0].runs.iter().map(|r| r.text.clone()).collect();
    assert!(full.contains("A-note."));
    assert!(full.contains("B-note."));
    Ok(())
}

/// Hardening: an agent and a human edit the SAME paragraph concurrently - the human directly
/// inserts while the agent proposes a tracked deletion on an overlapping span. After exchanging
/// snapshots both ways the two replicas converge identically (loro), and both the human's text and
/// the agent's still-pending redline survive.
#[test]
fn agent_and_human_racing_one_paragraph_converge() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("The quick brown fox.")], None)?;

    // The human edits directly on their own replica; the agent proposes on theirs, from the same base.
    let human = CollabDoc::new();
    human.merge(&base.snapshot()?)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;

    human.insert_text(0, 4, "very ", "human types")?; // "The very quick brown fox."
    let brown = agent.find("brown ", false)?[0].anchor.clone();
    agent.propose_delete(&brown, "2026-06-24T00:00:00Z", "redundant")?; // tracked

    // Exchange both ways and converge.
    let (h_snap, a_snap) = (human.snapshot()?, agent.export()?);
    agent.merge(&h_snap)?;
    human.merge(&a_snap)?;
    assert_eq!(agent.paragraphs()?, human.paragraphs()?, "replicas converged");

    // Both edits are present: the human's "very " (live) and the agent's tracked deletion of "brown ".
    let merged = agent.paragraphs()?;
    let full: String = merged[0].runs.iter().map(|r| r.text.clone()).collect();
    assert!(full.contains("very "), "human edit survived");
    let deleted = merged[0].runs.iter().find(|r| r.text == "brown ").expect("del run");
    assert!(deleted.track.is_some(), "agent's redline survived the race, still pending");

    // Accepting the agent's change yields the merged result, deterministically on both replicas.
    agent.accept_all()?;
    human.merge(&agent.export()?)?;
    assert_eq!(agent.paragraphs()?, human.paragraphs()?, "still converged after accept");
    let after: String = agent.paragraphs()?[0].runs.iter().map(|r| r.text.clone()).collect();
    assert_eq!(after, "The very quick fox.");
    Ok(())
}

/// The realistic agent loop: locate text by quote (`find`), propose a replacement against the
/// returned anchor, and have it land as an attributed redline (insertion + retained deletion).
/// Accepting everything yields the clean replacement.
#[test]
fn agent_find_and_replace_lands_as_attributed_redline() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("The cat sat.")], None)?;
    let agent = AgentPeer::join("AI Agent (editor)", &base.snapshot()?)?;

    let hits = agent.find("cat", false)?;
    assert_eq!(hits.len(), 1);
    let (del, ins) =
        agent.propose_replace(&hits[0].anchor, "dog", "2026-06-23T00:00:00Z", "clearer noun")?;
    assert!(del > 0 && ins > 0 && del != ins, "two distinct revisions");

    let runs = agent.paragraphs()?[0].runs.clone();
    let ins_run = runs.iter().find(|r| r.text == "dog").expect("inserted run");
    assert_eq!(ins_run.track.as_ref().unwrap().author, "AI Agent (editor)");
    let del_run = runs.iter().find(|r| r.text == "cat").expect("deleted text retained");
    assert!(del_run.track.is_some(), "old text marked deleted, not destroyed");

    agent.accept_all()?;
    let text: String = agent.paragraphs()?[0].runs.iter().map(|r| r.text.clone()).collect();
    assert_eq!(text, "The dog sat.");
    Ok(())
}

/// An agent can comment on a located range; the comment carries its text + agent attribution.
#[test]
fn agent_comments_on_a_located_range() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("The cat sat.")], None)?;
    let agent = AgentPeer::join("AI Agent (reviewer)", &base.snapshot()?)?;

    let range = agent.find("cat", false)?[0].anchor.clone();
    let id = agent.add_comment(&range, "Which cat?", "2026-06-23T00:00:00Z")?;
    let comments = agent.comments();
    let c = comments.iter().find(|c| c.id == id).expect("comment present");
    assert_eq!(c.text, "Which cat?");
    assert_eq!(c.author, "AI Agent (reviewer)");
    Ok(())
}

/// A held anchor whose content a human deleted (here by joining its block away) is refused at
/// propose-time with a clear "stale" error, instead of silently editing the wrong place.
#[test]
fn stale_anchor_is_refused() -> Result<()> {
    let human = CollabDoc::new();
    human.append_paragraph(&[Run::plain("First.")], None)?;
    human.append_paragraph(&[Run::plain("Second.")], None)?;
    let agent = AgentPeer::join("AI Agent", &human.snapshot()?)?;
    let at = agent.anchor(1, 0, Side::Right)?; // into the second paragraph

    human.join_paragraph(1, "human merged the paragraphs")?; // the anchored block is gone
    agent.merge(&human.snapshot()?)?;

    let err = agent
        .propose_insert(&at, "X", "2026-06-23T00:00:00Z", "note")
        .expect_err("a stale anchor must be refused");
    assert!(err.to_string().contains("stale"), "got: {err}");
    Ok(())
}

/// Loading a document with a table (the standalone path), the agent reads the table shape at an
/// anchored cell and proposes a tracked row insertion that grows the table.
#[test]
fn agent_proposes_a_tracked_table_row() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>Intro</w:t></w:r></w:p>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
  <w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
<w:p><w:r><w:t>Outro</w:t></w:r></w:p>
</w:body></w:document>"#;
    let agent = AgentPeer::from_document_xml("AI Agent", xml)?;
    let at = agent.anchor(1, 0, Side::Right)?; // para 1 = cell "A1"
    assert_eq!(agent.table_context(&at)?, Some((0, 0, 2, 2)), "2x2 at A1");

    let id = agent.propose_insert_table_row(&at, true, "2026-06-23T00:00:00Z", "extra row")?;
    assert!(id.is_some(), "row inserted inside the table");
    assert_eq!(agent.paragraphs()?.len(), 8, "two new empty cells added");
    assert_eq!(agent.table_context(&at)?, Some((0, 0, 3, 2)), "now 3 rows");
    Ok(())
}

/// The agent adds a hyperlink over a located range; `link_at` inside that range reports the target.
#[test]
fn agent_adds_a_hyperlink_and_bookmark() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("Visit OpenAI today.")], None)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;

    let range = agent.find("OpenAI", false)?[0].anchor.clone();
    agent.add_hyperlink(&range, "https://openai.com")?;
    let inside = agent.anchor(0, 8, Side::Right)?; // inside "OpenAI" (offsets 6..12)
    assert_eq!(
        agent.link_at(&inside)?.map(|(_, t)| t),
        Some("https://openai.com".to_string())
    );

    agent.add_bookmark(&range, "openai_ref")?; // succeeds (direct edit)
    Ok(())
}

/// Picture parity: the agent inserts a picture as an attributed tracked change, resizes + crops it
/// (direct guarded edits), and removes a *pre-existing* picture as a tracked deletion.
#[test]
fn agent_inserts_and_edits_a_picture() -> Result<()> {
    let png = vec![0x89u8, b'P', b'N', b'G', 1, 2, 3];
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("Figure here.")], None)?;
    // A pre-existing (accepted) picture, so the agent's tracked deletion isn't deleting its own
    // un-accepted insertion (which would just cancel, not stack a w:del).
    let existing = base.insert_image(0, 12, png.clone(), "image/png", 100, 100, "base picture")?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;

    // The agent inserts a NEW picture at the start, as a tracked insertion attributed to it.
    let at = agent.anchor(0, 0, Side::Right)?;
    let id =
        agent.insert_image(&at, png, "image/png", 914_400, 685_800, "2026-06-25T00:00:00Z", "add diagram")?;
    assert!(agent.image_placements().contains_key(&id) && id != existing);
    let run = agent.paragraphs()?[0].runs.iter().find(|r| r.image == Some(id)).cloned();
    assert_eq!(run.and_then(|r| r.track).map(|t| t.kind), Some(scriptor_crdt::TrackKind::Ins));

    // Geometry edits are direct (image geometry isn't a tracked change).
    assert!(agent.resize_image(id, 457_200, 342_900, "smaller")?);
    assert!(agent.crop_image(id, 5000, 0, 5000, 0, "trim sides")?);
    let pl = agent.image_placements()[&id].clone();
    assert_eq!((pl.w_emu, pl.h_emu, pl.crop_l, pl.crop_r), (457_200, 342_900, 5000, 5000));

    // Remove the pre-existing picture as a tracked deletion: its run is retained, marked del.
    assert!(agent.remove_image(existing, "2026-06-25T00:00:00Z", "drop it")?);
    let run = agent.paragraphs()?[0].runs.iter().find(|r| r.image == Some(existing)).cloned();
    assert_eq!(run.and_then(|r| r.track).map(|t| t.kind), Some(scriptor_crdt::TrackKind::Del));
    Ok(())
}

/// Perception loop: read the outline, read a node by its stable id, and confirm an anchor found by
/// quote maps back to that same node.
#[test]
fn agent_reads_outline_then_a_node_via_anchor() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("Introduction.")], None)?;
    base.append_paragraph(&[Run::plain("Body text here.")], None)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;

    let snap = agent.outline(60, 0, 0)?;
    assert_eq!(snap.nodes.len(), 2);
    assert_eq!(snap.nodes[1].preview, "Body text here.");

    let id = snap.nodes[1].node_id.clone();
    assert_eq!(agent.read_node(&id)?.expect("node lives").text, "Body text here.");

    let range = agent.find("Body", false)?[0].anchor.clone();
    assert_eq!(agent.node_at(&range.start)?, Some(id), "the quote's anchor maps to the node");
    Ok(())
}

/// A validated multi-op proposal applies as one batch: a replace (del+ins) and a comment land
/// together, attributed, with all change ids returned.
#[test]
fn proposal_applies_a_validated_batch_atomically() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("The cat sat on the mat.")], None)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;

    let cat = agent.find("cat", false)?[0].anchor.clone();
    let mat = agent.find("mat", false)?[0].anchor.clone();
    let proposal = Proposal {
        base_revision: agent.revision(),
        title: "tighten wording".into(),
        ops: vec![
            ProposalOp::Replace { range: cat, text: "dog".into() },
            ProposalOp::Comment { range: mat, text: "define 'mat'".into() },
        ],
    };

    let ids = match agent.submit_proposal(&proposal, "2026-06-23T00:00:00Z")? {
        ProposalResult::Applied { change_ids, .. } => change_ids,
        other => panic!("expected Applied, got {other:?}"),
    };
    assert_eq!(ids.len(), 3, "replace = del + ins, comment = 1");

    let runs = agent.paragraphs()?[0].runs.clone();
    assert!(runs.iter().any(|r| r.text == "dog" && r.track.is_some()), "redline landed");
    assert!(agent.comments().iter().any(|c| c.text == "define 'mat'"), "comment landed");
    Ok(())
}

/// A proposal built against an older revision is rejected (a concurrent edit moved the doc) and
/// nothing applies.
#[test]
fn proposal_rejects_a_stale_base_revision() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("Hello there.")], None)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;

    let at = agent.find("Hello", false)?[0].anchor.clone();
    let stale = agent.revision();
    agent.append_paragraph(&[Run::plain("Added.")], None)?; // bumps the revision

    let proposal = Proposal {
        base_revision: stale,
        title: "note".into(),
        ops: vec![ProposalOp::Comment { range: at, text: "hi".into() }],
    };
    match agent.submit_proposal(&proposal, "2026-06-23T00:00:00Z")? {
        ProposalResult::Stale { current } => assert!(current > stale),
        other => panic!("expected Stale, got {other:?}"),
    }
    assert!(agent.comments().is_empty(), "nothing applied on a stale proposal");
    Ok(())
}

/// One invalid op aborts the whole proposal before anything applies (all-or-nothing).
#[test]
fn proposal_is_all_or_nothing_on_an_invalid_op() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("The cat sat.")], None)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;
    let cat = agent.find("cat", false)?[0].anchor.clone();

    // A foreign anchor (from a different document) cannot resolve here.
    let other = CollabDoc::new();
    other.append_paragraph(&[Run::plain("Elsewhere.")], None)?;
    let foreign = other.anchor(0, 0, Side::Right)?;

    let proposal = Proposal {
        base_revision: agent.revision(),
        title: "mixed".into(),
        ops: vec![
            ProposalOp::Replace { range: cat, text: "dog".into() }, // valid
            ProposalOp::Insert { at: foreign, text: "X".into() },   // invalid
        ],
    };
    match agent.submit_proposal(&proposal, "2026-06-23T00:00:00Z")? {
        ProposalResult::Invalid { index, .. } => assert_eq!(index, 1),
        other => panic!("expected Invalid, got {other:?}"),
    }
    assert!(agent.list_changes()?.is_empty(), "the valid op must not have applied");
    Ok(())
}

/// True all-or-nothing across an APPLY-TIME failure (audit H8): a self-overlapping move passes the
/// validate-first pass (its anchors resolve) but fails when applied. The trial pass catches it, so
/// the earlier valid op in the same batch is rolled back too - the document is left untouched.
#[test]
fn proposal_is_atomic_on_an_apply_time_failure() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("The quick brown fox jumps.")], None)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;

    let here = agent.find("fox", false)?[0].anchor.start.clone();
    let src = agent.find("quick brown", false)?[0].anchor.clone(); // 4..15
    let dest = agent.anchor(0, 8, Side::Right)?; // inside the source range -> apply fails

    let proposal = Proposal {
        base_revision: agent.revision(),
        title: "batch".into(),
        ops: vec![
            ProposalOp::Insert { at: here, text: "fast ".into() }, // valid
            ProposalOp::Move { from: src, to: dest },              // valid anchors, fails to apply
        ],
    };
    match agent.submit_proposal(&proposal, "2026-06-24T00:00:00Z")? {
        ProposalResult::Invalid { index, .. } => assert_eq!(index, 1, "the move is the failing op"),
        other => panic!("expected Invalid, got {other:?}"),
    }
    // Nothing applied - not even the valid insert that preceded the failing op.
    assert!(agent.list_changes()?.is_empty(), "the batch rolled back entirely");
    let text: String = agent.paragraphs()?[0].runs.iter().map(|r| r.text.clone()).collect();
    assert_eq!(text, "The quick brown fox jumps.", "document untouched");
    Ok(())
}

/// A policy that forbids removing text: it refuses a direct delete and makes a proposal containing
/// a (delete-bearing) replace Invalid - while leaving allowed actions through.
struct NoDeletions;
impl AgentPolicy for NoDeletions {
    fn authorize(&self, action: &AgentAction, _node_id: Option<NodeId>) -> Decision {
        match action {
            AgentAction::Delete | AgentAction::Replace => {
                Decision::Deny("this agent may not remove text".into())
            }
            _ => Decision::Allow,
        }
    }
}

/// A content-aware policy: refuse any edit that targets a specific (protected) node.
struct ProtectNode(NodeId);
impl AgentPolicy for ProtectNode {
    fn authorize(&self, _action: &AgentAction, node_id: Option<NodeId>) -> Decision {
        match node_id {
            Some(n) if n == self.0 => Decision::Deny("this paragraph is protected".into()),
            _ => Decision::Allow,
        }
    }
}

#[test]
fn policy_vetoes_a_forbidden_action_and_proposal_op() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("The cat sat.")], None)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?.add_policy(Box::new(NoDeletions));
    let cat = agent.find("cat", false)?[0].anchor.clone();

    // A direct delete is refused outright.
    assert!(agent.propose_delete(&cat, "2026-06-23T00:00:00Z", "drop").is_err());
    // An allowed action (comment) still works.
    agent.add_comment(&cat, "which cat?", "2026-06-23T00:00:00Z")?;

    // A proposal whose op is forbidden is Invalid - and nothing applies.
    let proposal = Proposal {
        base_revision: agent.revision(),
        title: "rename".into(),
        ops: vec![ProposalOp::Replace { range: cat, text: "dog".into() }],
    };
    match agent.submit_proposal(&proposal, "2026-06-23T00:00:00Z")? {
        ProposalResult::Invalid { index, .. } => assert_eq!(index, 0),
        other => panic!("expected Invalid, got {other:?}"),
    }
    assert!(agent.list_changes()?.is_empty(), "no tracked change should have landed");
    Ok(())
}

/// A content-aware policy refuses any edit targeting a protected node, while allowing edits
/// elsewhere - the canonical "don't touch this clause" rule the bare-verb policy couldn't express.
#[test]
fn content_aware_policy_protects_a_node() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("Editable intro.")], None)?;
    base.append_paragraph(&[Run::plain("Protected clause.")], None)?;
    let protected = {
        let probe = AgentPeer::join("probe", &base.snapshot()?)?;
        probe.node_id(1).expect("node 1 exists")
    };
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?
        .add_policy(Box::new(ProtectNode(protected)));

    // Editing the protected node is refused...
    let in_clause = agent.find("Protected", false)?[0].anchor.clone();
    assert!(agent.propose_delete(&in_clause, "2026-06-23T00:00:00Z", "x").is_err());
    // ...but editing elsewhere is fine.
    let in_intro = agent.find("intro", false)?[0].anchor.clone();
    agent.propose_replace(&in_intro, "opening", "2026-06-23T00:00:00Z", "reword")?;
    Ok(())
}

/// An event sink records every action the agent performs (the integrator's audit feed), enriched
/// with the target node, the rationale, and the human principal.
struct Recorder {
    log: std::sync::Arc<std::sync::Mutex<Vec<AgentEvent>>>,
}
impl EventSink for Recorder {
    fn emit(&self, event: &AgentEvent) {
        self.log.lock().unwrap().push(event.clone());
    }
}

#[test]
fn event_carries_node_principal_and_rationale() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("The cat sat.")], None)?;
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?
        .on_behalf_of("alice@example.com")
        .add_sink(Box::new(Recorder { log: log.clone() }));

    let cat = agent.find("cat", false)?[0].anchor.clone();
    let node = agent.node_at(&cat.start)?;
    agent.propose_replace(&cat, "dog", "2026-06-23T00:00:00Z", "clearer noun")?;

    let events = log.lock().unwrap().clone();
    let ev = events.iter().find(|e| e.action == AgentAction::Replace).expect("replace observed");
    assert_eq!(ev.on_behalf_of.as_deref(), Some("alice@example.com"));
    assert_eq!(ev.node_id, node, "event carries the target node");
    assert_eq!(ev.rationale.as_deref(), Some("clearer noun"));
    Ok(())
}

/// The full JSON wire round-trip a non-Rust integrator uses: read perception out as JSON, take an
/// opaque anchor token from a find hit, build a proposal as JSON, deserialize it, submit, and see
/// the redline land.
#[test]
fn wire_json_round_trip_drives_a_redline() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("The cat sat.")], None)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;

    // Perception out as JSON.
    let outline_json = serde_json::to_string(&agent.outline_dto(80, 0, 0)?).unwrap();
    assert!(outline_json.contains("\"revision\""));
    assert!(outline_json.contains("The cat sat."));

    // Find -> an opaque anchor token (the agent never builds an anchor).
    let hits = agent.find_dto("cat", false)?;
    assert_eq!(hits.len(), 1);
    let range = hits[0].anchor.clone();

    // Build the proposal as JSON, round-trip it through serde, submit via the wire method.
    let proposal = wire::ProposalDto {
        base_revision: agent.revision(),
        title: "clearer noun".into(),
        ops: vec![wire::ProposalOpDto::Replace { range, text: "dog".into() }],
    };
    let json = serde_json::to_string(&proposal).unwrap();
    let decoded: wire::ProposalDto = serde_json::from_str(&json).unwrap();
    match agent.submit_proposal_dto(decoded, "2026-06-23T00:00:00Z")? {
        wire::ProposalResultDto::Applied { change_ids, .. } => assert_eq!(change_ids.len(), 2),
        other => panic!("expected Applied, got {other:?}"),
    }

    agent.accept_all()?;
    let text: String = agent.paragraphs()?[0].runs.iter().map(|r| r.text.clone()).collect();
    assert_eq!(text, "The dog sat.");
    Ok(())
}

/// Wire parity for the rest of the agent loop: build an anchor over the wire (no prior find), submit
/// a Split op (newly mirrored), read comment bodies via comments_dto, resolve a token, and observe a
/// peer's change via merge_observed_dto - all through the JSON DTOs a non-Rust integrator uses.
#[test]
fn wire_covers_split_anchors_comments_and_observations() -> Result<()> {
    let human = CollabDoc::new();
    human.append_paragraph(&[Run::plain("One two three.")], None)?;
    let agent = AgentPeer::join("AI Agent", &human.snapshot()?)?;

    // Anchor built over the wire, then a Split op (round-tripped through JSON).
    let at = agent.anchor_dto(0, 4, "right")?; // before "two"
    let proposal = wire::ProposalDto {
        base_revision: agent.revision(),
        title: "split".into(),
        ops: vec![wire::ProposalOpDto::Split { at }],
    };
    let decoded: wire::ProposalDto =
        serde_json::from_str(&serde_json::to_string(&proposal).unwrap()).unwrap();
    match agent.submit_proposal_dto(decoded, "2026-06-24T00:00:00Z")? {
        wire::ProposalResultDto::Applied { .. } => {}
        other => panic!("expected Applied, got {other:?}"),
    }

    // A comment via the wire, read back as a CommentDto.
    let range = agent.find_dto("three", false)?[0].anchor.clone();
    let cprop = wire::ProposalDto {
        base_revision: agent.revision(),
        title: "note".into(),
        ops: vec![wire::ProposalOpDto::Comment { range, text: "define".into() }],
    };
    agent.submit_proposal_dto(cprop, "2026-06-24T00:00:00Z")?;
    assert!(agent.comments_dto().iter().any(|c| c.text == "define"), "comment readable over the wire");

    // Resolve a token + map it to a node, over the wire.
    let tok = agent.anchor_dto(0, 0, "right")?;
    assert!(matches!(agent.resolve_dto(&tok)?, wire::ResolvedDto::Live { .. }));
    assert!(agent.node_at_dto(&tok)?.is_some());

    // Observe another peer's change as ObservationDto.
    let reviewer = AgentPeer::join("Reviewer", &agent.export()?)?;
    reviewer.propose_insertion(0, 0, "X", "2026-06-24T00:00:00Z", "r")?;
    let obs = agent.merge_observed_dto(&reviewer.export()?)?;
    assert!(
        obs.iter().any(|o| matches!(o, wire::ObservationDto::ChangeAdded { .. })),
        "the peer's change was observed over the wire: {obs:?}"
    );
    Ok(())
}

/// Wire parity for the direct anchored ops: read a table's shape, propose a tracked row, and add a
/// hyperlink + bookmark + read the link back - all via anchor tokens + DTOs.
#[test]
fn wire_covers_tables_and_links() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>Intro</w:t></w:r></w:p>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
  <w:tr><w:tc><w:p><w:r><w:t>A2</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B2</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
<w:p><w:r><w:t>Outro</w:t></w:r></w:p>
</w:body></w:document>"#;
    let agent = AgentPeer::from_document_xml("AI Agent", xml)?;

    // A hyperlink + bookmark over "A1" (para 1), then read the link back, all over the wire.
    let range = agent.find_dto("A1", false)?[0].anchor.clone();
    agent.add_hyperlink_dto(&range, "https://example.com")?;
    let inside = agent.anchor_dto(1, 1, "right")?; // inside "A1"
    assert_eq!(
        agent.link_at_dto(&inside)?.map(|l| l.target),
        Some("https://example.com".to_string())
    );
    agent.add_bookmark_dto(&range, "ref1")?;

    // Table shape + a tracked row insertion, by token.
    let cell = agent.anchor_dto(1, 0, "right")?;
    assert_eq!(agent.table_context_dto(&cell)?, Some([0, 0, 2, 2]), "2x2 at A1");
    assert!(
        agent.propose_insert_table_row_dto(&cell, true, "2026-06-24T00:00:00Z", "extra")?.is_some()
    );
    assert_eq!(agent.table_context_dto(&cell)?, Some([0, 0, 3, 2]), "now 3 rows");
    Ok(())
}

/// Wire parity for the header/footer region: read the header over the wire, find + redline it via
/// region DTOs, and accept - the header story driven entirely through JSON.
#[test]
fn wire_covers_the_header_region() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("Body.")], None)?;
    let mut doc = CollabDoc::from_docx_bytes(&base.to_docx_bytes()?)?;
    doc.set_header_text("Confidential draft");
    let agent = AgentPeer::from_docx_bytes("AI Agent", &doc.to_docx_bytes()?)?;

    assert_eq!(agent.region_text("header")?, "Confidential draft");
    let hit = agent.region_find_dto("header", "draft", false)?[0].anchor.clone();
    let [del, ins] =
        agent.region_propose_replace_dto("header", &hit, "release", "2026-06-24T00:00:00Z", "status")?;
    assert!(del != ins, "distinct del + ins ids in the header");
    assert_eq!(agent.region_list_changes_dto("header")?.len(), 2, "two changes pending in the header");

    agent.region_accept_all_dto("header")?;
    assert_eq!(agent.region_text("header")?, "Confidential release");
    // An unknown region is a clear error.
    assert!(agent.region_text("sidebar").is_err());
    Ok(())
}

#[test]
fn event_sink_records_agent_actions() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("The cat sat.")], None)?;
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?
        .add_sink(Box::new(Recorder { log: log.clone() }));

    let cat = agent.find("cat", false)?[0].anchor.clone();
    agent.propose_replace(&cat, "dog", "2026-06-23T00:00:00Z", "clearer noun")?;
    agent.add_comment(&cat, "why?", "2026-06-23T00:00:00Z")?;

    let recorded: Vec<AgentAction> = log.lock().unwrap().iter().map(|e| e.action).collect();
    assert!(recorded.contains(&AgentAction::Replace), "replace was observed");
    assert!(recorded.contains(&AgentAction::AddComment), "comment was observed");
    Ok(())
}

/// The read -> edit bridge: read a node, build a sub-range anchor from the node id (no re-find),
/// and redline it.
#[test]
fn anchor_in_node_addresses_a_read_node() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("The cat sat.")], None)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;

    let node = agent.outline(80, 0, 0)?.nodes[0].node_id.clone();
    let content = agent.read_node(&node)?.expect("node");
    // "cat" is codepoints 4..7 of the read text - address it directly from the node id.
    let pos = content.text.find("cat").unwrap();
    let range = agent.anchor_range_in_node(&node, pos, pos + 3)?;
    agent.propose_replace(&range, "dog", "2026-06-23T00:00:00Z", "noun")?;
    agent.accept_all()?;
    let text: String = agent.paragraphs()?[0].runs.iter().map(|r| r.text.clone()).collect();
    assert_eq!(text, "The dog sat.");
    Ok(())
}

/// A proposal can now carry numbering / split / join ops (parity with the direct API).
#[test]
fn proposal_carries_numbering_split_join() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("One sentence. Two sentence.")], None)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;
    let at = agent.find("Two", false)?[0].anchor.start.clone();

    let proposal = Proposal {
        base_revision: agent.revision(),
        title: "split into two paragraphs".into(),
        ops: vec![ProposalOp::Split { at }],
    };
    match agent.submit_proposal(&proposal, "2026-06-23T00:00:00Z")? {
        ProposalResult::Applied { .. } => {}
        other => panic!("expected Applied, got {other:?}"),
    }
    // The split landed as a tracked change.
    assert!(agent.list_changes()?.iter().any(|c| c.kind == "ins"));
    Ok(())
}

/// Selective triage: accept only the changes authored by a given author. Changes are applied
/// through one authority (the reviewer), as an integrator would, so revision ids stay distinct.
#[test]
fn accept_by_author_resolves_only_that_authors_changes() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("The cat sat on the mat.")], None)?;
    let review = AgentPeer::join("Reviewer", &base.snapshot()?)?;

    // Alice proposes from base; the reviewer merges.
    let alice = AgentPeer::join("Alice", &base.snapshot()?)?;
    let cat = alice.find("cat", false)?[0].anchor.clone();
    alice.propose_replace(&cat, "dog", "2026-06-23T00:00:00Z", "a")?;
    review.merge(&alice.export()?)?;

    // Bob proposes from the reviewer's current state (ids continue past Alice's); merge back.
    let bob = AgentPeer::join("Bob", &review.export()?)?;
    let mat = bob.find("mat", false)?[0].anchor.clone();
    bob.propose_replace(&mat, "rug", "2026-06-23T00:00:00Z", "b")?;
    review.merge(&bob.export()?)?;

    assert_eq!(review.list_changes()?.len(), 4, "2 replaces = 4 tracked changes");
    let resolved = review.accept_by_author("Alice")?;
    assert_eq!(resolved, 2, "only Alice's del+ins resolved");
    assert!(review.list_changes()?.iter().all(|c| c.author == "Bob"), "Bob's remain pending");
    Ok(())
}

/// A comment spanning a paragraph boundary: the agent locates a quote in the first paragraph and an
/// end in the second, builds a cross-paragraph range, and the comment anchors over both - the most
/// common multi-paragraph review action (mark a whole clause).
#[test]
fn comment_spans_a_paragraph_boundary() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("First paragraph here.")], None)?;
    base.append_paragraph(&[Run::plain("Second paragraph here.")], None)?;
    let agent = AgentPeer::join("AI Agent (reviewer)", &base.snapshot()?)?;

    // Range: from "paragraph" in para 0 to the end of "Second" in para 1.
    let start = agent.find("paragraph", false)?[0].anchor.start.clone(); // para 0
    let end = {
        let m = agent.find("Second", false)?[0].anchor.clone(); // para 1
        m.end
    };
    let range = AnchorRange { start, end };

    // Sanity: it really straddles two paragraphs (single-para resolve would refuse it).
    assert!(agent.doc.resolve_range(&range).is_none(), "range is genuinely multi-paragraph");

    let id = agent.add_comment(&range, "This clause needs work.", "2026-06-23T00:00:00Z")?;
    let c = agent.comments().into_iter().find(|c| c.id == id).expect("comment present");
    assert_eq!(c.text, "This clause needs work.");

    // The anchor covers both paragraphs: present at the start point in para 0 and in para 1.
    assert!(agent.comments_at(0, 10)?.contains(&id), "anchored in the first paragraph");
    assert!(agent.comments_at(1, 0)?.contains(&id), "anchored in the second paragraph");
    Ok(())
}

/// A tracked deletion crossing a paragraph boundary: it lands under one revision id (one entry in
/// list_changes), retains all text until reviewed, and on accept removes the spanned text AND merges
/// the two paragraphs into one (text[..start] + text[end..]).
#[test]
fn redline_deletes_across_a_paragraph_boundary() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("Alpha beta gamma.")], None)?;
    base.append_paragraph(&[Run::plain("Delta epsilon zeta.")], None)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;

    // From "beta" (para 0) through the end of "epsilon" (para 1).
    let start = agent.find("beta", false)?[0].anchor.start.clone();
    let end = agent.find("epsilon", false)?[0].anchor.end.clone();
    let range = AnchorRange { start, end };
    assert!(agent.doc.resolve_range(&range).is_none(), "range is genuinely multi-paragraph");

    agent.propose_delete(&range, "2026-06-24T00:00:00Z", "tighten")?;
    // One logical change, text retained, still two paragraphs.
    assert_eq!(agent.list_changes()?.len(), 1, "a multi-paragraph delete is one revision");
    assert_eq!(agent.paragraphs()?.len(), 2, "nothing merged until accepted");

    agent.accept_all()?;
    let paras = agent.paragraphs()?;
    assert_eq!(paras.len(), 1, "accepting the delete merged the paragraphs");
    let text: String = paras[0].runs.iter().map(|r| r.text.clone()).collect();
    assert_eq!(text, "Alpha  zeta.");
    Ok(())
}

/// Rejecting a multi-paragraph deletion restores both paragraphs intact (text + the boundary ¶).
#[test]
fn rejecting_a_multi_paragraph_delete_restores_both_paragraphs() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("Alpha beta gamma.")], None)?;
    base.append_paragraph(&[Run::plain("Delta epsilon zeta.")], None)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;

    let start = agent.find("beta", false)?[0].anchor.start.clone();
    let end = agent.find("epsilon", false)?[0].anchor.end.clone();
    agent.propose_delete(&AnchorRange { start, end }, "2026-06-24T00:00:00Z", "x")?;

    agent.reject_all()?;
    let paras = agent.paragraphs()?;
    assert_eq!(paras.len(), 2, "reject keeps the paragraphs separate");
    let p0: String = paras[0].runs.iter().map(|r| r.text.clone()).collect();
    let p1: String = paras[1].runs.iter().map(|r| r.text.clone()).collect();
    assert_eq!(p0, "Alpha beta gamma.");
    assert_eq!(p1, "Delta epsilon zeta.");
    assert!(agent.list_changes()?.is_empty(), "no change remains pending");
    Ok(())
}

/// A replace whose range spans paragraphs: the deletion merges the paragraphs and the new text is
/// inserted at the range start, so accepting yields text[..start] + new + text[end..] in one paragraph.
#[test]
fn redline_replaces_across_a_paragraph_boundary() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("Alpha beta gamma.")], None)?;
    base.append_paragraph(&[Run::plain("Delta epsilon zeta.")], None)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;

    let start = agent.find("beta", false)?[0].anchor.start.clone();
    let end = agent.find("epsilon", false)?[0].anchor.end.clone();
    let (del, ins) = agent.propose_replace(
        &AnchorRange { start, end },
        "MIDDLE",
        "2026-06-24T00:00:00Z",
        "reword",
    )?;
    assert!(del != ins, "distinct deletion + insertion ids");

    agent.accept_all()?;
    let paras = agent.paragraphs()?;
    assert_eq!(paras.len(), 1);
    let text: String = paras[0].runs.iter().map(|r| r.text.clone()).collect();
    assert_eq!(text, "Alpha MIDDLE zeta.");
    Ok(())
}

/// Perception enrichment: the outline carries table coordinates for cells, find flags deleted text,
/// comment_locations reports where comments sit, and a run's annotations (hyperlink / comment) ride
/// on its DTO so a wire agent won't blindly clobber them.
#[test]
fn perception_surfaces_tables_links_comments_and_deletions() -> Result<()> {
    let xml = br#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:r><w:t>Intro</w:t></w:r></w:p>
<w:tbl>
  <w:tblGrid><w:gridCol w:w="5000"/><w:gridCol w:w="5000"/></w:tblGrid>
  <w:tr><w:tc><w:p><w:r><w:t>A1</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>B1</w:t></w:r></w:p></w:tc></w:tr>
</w:tbl>
<w:p><w:r><w:t>Outro</w:t></w:r></w:p>
</w:body></w:document>"#;
    let agent = AgentPeer::from_document_xml("AI Agent", xml)?;

    // Outline: the cell nodes carry (row, col, n_rows, n_cols); a plain paragraph does not.
    let snap = agent.outline_dto(40, 0, 0)?;
    let cell = snap.nodes.iter().find(|n| n.preview == "A1").expect("cell A1");
    assert_eq!(cell.table, Some([0, 0, 1, 2]), "A1 is row 0, col 0 of a 1x2 table");
    let intro = snap.nodes.iter().find(|n| n.preview == "Intro").expect("intro");
    assert_eq!(intro.table, None, "a body paragraph has no table coords");

    // A hyperlink over "Intro" rides on the run DTO.
    let range = agent.find("Intro", false)?[0].anchor.clone();
    agent.add_hyperlink(&range, "https://example.com")?;
    let node = agent.node_id(0).unwrap();
    let dto = agent.read_node_dto(&node.to_string())?.expect("node");
    assert!(dto.runs.iter().any(|r| r.link.is_some()), "the hyperlink run is flagged on the wire");

    // A comment's location is reported and pairs with its body by id.
    let cid = agent.add_comment(&range, "define this", "2026-06-24T00:00:00Z")?;
    let loc = agent.comment_locations()?.into_iter().find(|l| l.id == cid).expect("comment located");
    assert_eq!(loc.start_para, 0);
    Ok(())
}

/// An observation sink records what other peers did across a merge.
struct ObsRecorder {
    log: std::sync::Arc<std::sync::Mutex<Vec<Observation>>>,
}
impl ObservationSink for ObsRecorder {
    fn observe(&self, obs: &Observation) {
        self.log.lock().unwrap().push(obs.clone());
    }
}

/// The agent's feedback loop: it proposes a replacement, a human accepts everything, and on the
/// next merge the agent observes both halves resolved as ACCEPTED (and the sink is notified).
#[test]
fn agent_observes_a_human_accepting_its_suggestion() -> Result<()> {
    let human = CollabDoc::new();
    human.append_paragraph(&[Run::plain("The cat sat.")], None)?;
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = AgentPeer::join("AI Agent", &human.snapshot()?)?
        .add_observation_sink(Box::new(ObsRecorder { log: log.clone() }));

    let cat = agent.find("cat", false)?[0].anchor.clone();
    let (del, ins) = agent.propose_replace(&cat, "dog", "2026-06-24T00:00:00Z", "noun")?;

    // The human merges the agent's suggestion and accepts all of it.
    human.merge(&agent.export()?)?;
    human.accept_all("human accepts")?;

    // The agent merges the human's reply and observes the outcome.
    let obs = agent.merge_observed(&human.snapshot()?)?;
    let accepted: Vec<u64> = obs
        .iter()
        .filter_map(|o| match o {
            Observation::ChangeResolved { id, accepted: Some(true), .. } => Some(*id),
            _ => None,
        })
        .collect();
    assert!(accepted.contains(&del), "the deletion was observed accepted");
    assert!(accepted.contains(&ins), "the insertion was observed accepted");
    assert_eq!(log.lock().unwrap().len(), obs.len(), "the sink saw every observation");
    Ok(())
}

/// The agent observes a human REJECTING its suggestion (both halves resolved as not-accepted).
#[test]
fn agent_observes_a_human_rejecting_its_suggestion() -> Result<()> {
    let human = CollabDoc::new();
    human.append_paragraph(&[Run::plain("The cat sat.")], None)?;
    let agent = AgentPeer::join("AI Agent", &human.snapshot()?)?;

    let cat = agent.find("cat", false)?[0].anchor.clone();
    let (del, ins) = agent.propose_replace(&cat, "dog", "2026-06-24T00:00:00Z", "noun")?;

    human.merge(&agent.export()?)?;
    human.reject_all("human rejects")?;

    let obs = agent.merge_observed(&human.snapshot()?)?;
    let rejected: Vec<u64> = obs
        .iter()
        .filter_map(|o| match o {
            Observation::ChangeResolved { id, accepted: Some(false), .. } => Some(*id),
            _ => None,
        })
        .collect();
    assert!(rejected.contains(&del) && rejected.contains(&ins), "both halves observed rejected");
    // The text reverted to the original.
    let text: String = agent.paragraphs()?[0].runs.iter().map(|r| r.text.clone()).collect();
    assert_eq!(text, "The cat sat.");
    Ok(())
}

/// The agent observes a human's OWN new tracked change arriving (not its own authorship).
#[test]
fn agent_observes_a_human_added_change() -> Result<()> {
    let human = CollabDoc::new();
    human.append_paragraph(&[Run::plain("The cat sat.")], None)?;
    let agent = AgentPeer::join("AI Agent", &human.snapshot()?)?;

    // A human reviewer (their own peer) proposes an insertion, then the agent merges it.
    let reviewer = AgentPeer::join("Reviewer Jane", &human.snapshot()?)?;
    reviewer.propose_insertion(0, 4, "big ", "2026-06-24T00:00:00Z", "emphasis")?;

    let obs = agent.merge_observed(&reviewer.export()?)?;
    assert!(
        obs.iter().any(|o| matches!(o, Observation::ChangeAdded { author, .. } if author == "Reviewer Jane")),
        "the human's new change was observed: {obs:?}"
    );
    Ok(())
}

/// The agent perceives + redlines the HEADER story through a region view: it reads the header text,
/// proposes a replacement there, and the change lands in the header (not the body) and resolves -
/// reusing the peer's identity + governance.
#[test]
fn agent_redlines_the_header_region() -> Result<()> {
    // Seed a real .docx (round-tripped so it has source parts), then add a header and re-save.
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("Body text.")], None)?;
    let mut doc = CollabDoc::from_docx_bytes(&base.to_docx_bytes()?)?;
    doc.set_header_text("Confidential draft");
    let agent = AgentPeer::from_docx_bytes("AI Agent", &doc.to_docx_bytes()?)?;

    assert!(agent.has_region(Region::Header), "header is present");
    let header = agent.region(Region::Header)?;
    assert_eq!(header.text()?, "Confidential draft");

    // Redline "draft" -> "release" in the header only.
    let hit = header.find("draft", false)?[0].anchor.clone();
    header.propose_replace(&hit, "release", "2026-06-24T00:00:00Z", "status changed")?;
    assert_eq!(header.list_changes()?.len(), 2, "del + ins landed in the header");
    assert!(agent.list_changes()?.is_empty(), "the body story is untouched");

    header.accept_all()?;
    assert_eq!(header.text()?, "Confidential release");
    Ok(())
}

/// A header edit is governed by the peer's policies + observed by its sinks, exactly like a body
/// edit (governance is the peer's, not per-region).
#[test]
fn header_region_edits_are_governed_and_observed() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("Body.")], None)?;
    let mut doc = CollabDoc::from_docx_bytes(&base.to_docx_bytes()?)?;
    doc.set_header_text("Old footer note");
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = AgentPeer::from_docx_bytes("AI Agent", &doc.to_docx_bytes()?)?
        .add_policy(Box::new(NoDeletions))
        .add_sink(Box::new(Recorder { log: log.clone() }));

    let header = agent.region(Region::Header)?;
    let note = header.find("note", false)?[0].anchor.clone();

    // The NoDeletions policy vetoes a delete in the header too.
    assert!(header.propose_delete(&note, "2026-06-24T00:00:00Z", "drop").is_err());
    // An allowed action (insert) goes through and is observed.
    let at = header.anchor(0, 0, Side::Left)?;
    header.propose_insert(&at, "DRAFT ", "2026-06-24T00:00:00Z", "mark draft")?;

    let observed: Vec<AgentAction> = log.lock().unwrap().iter().map(|e| e.action).collect();
    assert!(observed.contains(&AgentAction::Insert), "the header insert was observed");
    Ok(())
}

/// The agent moves a span that crosses a paragraph boundary (locate by quote → multi-paragraph
/// move) and, on accept, the moved content lands at the destination while the source is merged away.
#[test]
fn agent_moves_a_cross_paragraph_span() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("AAA BBB")], None)?;
    base.append_paragraph(&[Run::plain("CCC DDD")], None)?;
    base.append_paragraph(&[Run::plain("ZZZ")], None)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;

    let start = agent.find("BBB", false)?[0].anchor.start.clone(); // (0, 4)
    let end = agent.find("CCC", false)?[0].anchor.end.clone(); // (1, 3)
    let from = AnchorRange { start, end };
    assert!(agent.doc.resolve_range(&from).is_none(), "the move source is genuinely multi-paragraph");
    let to = agent.anchor(2, 3, Side::Right)?; // end of "ZZZ"

    let id = agent.propose_move(&from, &to, "2026-06-24T00:00:00Z", "reorg")?;
    assert!(id > 0);

    agent.accept_all()?;
    let texts: Vec<String> = agent
        .paragraphs()?
        .iter()
        .map(|p| p.runs.iter().map(|r| r.text.clone()).collect())
        .collect();
    assert_eq!(texts, ["AAA  DDD", "ZZZBBB", "CCC"]);
    Ok(())
}

/// A multi-paragraph comment also works through the proposal path (validate-first accepts a
/// cross-paragraph comment op rather than rejecting it as torn).
#[test]
fn proposal_accepts_a_multi_paragraph_comment() -> Result<()> {
    let base = CollabDoc::new();
    base.append_paragraph(&[Run::plain("Alpha line.")], None)?;
    base.append_paragraph(&[Run::plain("Beta line.")], None)?;
    let agent = AgentPeer::join("AI Agent", &base.snapshot()?)?;

    let start = agent.find("Alpha", false)?[0].anchor.start.clone();
    let end = agent.find("Beta", false)?[0].anchor.end.clone();
    let range = AnchorRange { start, end };

    let proposal = Proposal {
        base_revision: agent.revision(),
        title: "span comment".into(),
        ops: vec![ProposalOp::Comment { range, text: "spans both".into() }],
    };
    match agent.submit_proposal(&proposal, "2026-06-23T00:00:00Z")? {
        ProposalResult::Applied { change_ids, .. } => assert_eq!(change_ids.len(), 1),
        other => panic!("expected Applied, got {other:?}"),
    }
    assert!(agent.comments().iter().any(|c| c.text == "spans both"));
    Ok(())
}
