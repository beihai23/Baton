import { useCallback, useEffect, useRef, useState } from "react";
import {
  api, API_BASE, AgentInfo, AgentSession, AppNotification, Approval, BoardState, CardDetail,
  CardSummary, Comment, ListWithCards, LIST_NAMES, Member, MEMBER_NAMES, Project,
} from "./api";

const memberName = (id?: string | null) => (id ? MEMBER_NAMES[id] ?? id : "—");

type Tab = "讨论" | "需求" | "Git" | "现场" | "移交" | "产物";

export default function App() {
  const [board, setBoard] = useState<BoardState | null>(null);
  const [projects, setProjects] = useState<Project[]>([]);
  const [boardId, setBoardId] = useState<string | null>(null); // null = 默认第一个看板
  const [selected, setSelected] = useState<CardDetail | null>(null);
  const [approvals, setApprovals] = useState<Approval[]>([]);
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [sessions, setSessions] = useState<AgentSession[]>([]);
  const [members, setMembers] = useState<Member[]>([]);
  const [notifications, setNotifications] = useState<AppNotification[]>([]);
  const [showApprovals, setShowApprovals] = useState(false);
  const [showNotifs, setShowNotifs] = useState(false);
  const [showAgents, setShowAgents] = useState(false);
  const [showInstall, setShowInstall] = useState(false); // 接入指引面板（onboarding）
  // 引导横幅关闭记忆：用户关过就不再打扰（除非主动点接入入口）
  const [installDismissed, setInstallDismissed] = useState(
    () => localStorage.getItem("baton.install_dismissed") === "1",
  );
  // 已读游标（F-404）：本地存储 last_read_seq，无需服务端状态
  const [lastRead, setLastRead] = useState<number>(
    () => Number(localStorage.getItem("baton.last_read_seq") ?? 0),
  );
  const [newTitle, setNewTitle] = useState("");
  const [newProject, setNewProject] = useState("");
  const [newTpl, setNewTpl] = useState("software");
  const [projFormOpen, setProjFormOpen] = useState(false); // 新建项目表单（低频动作，默认收起）
  const [editingProj, setEditingProj] = useState<string | null>(null); // 行内重命名的项目 id
  const [editName, setEditName] = useState("");
  const [delArming, setDelArming] = useState<string | null>(null); // 二次确认删除的项目 id
  const [dragging, setDragging] = useState<CardSummary | null>(null); // 正在拖拽的卡片（列策略预告用）
  const [toast, setToast] = useState<string | null>(null);
  const [toastKind, setToastKind] = useState<"error" | "info">("info");
  const selectedIdRef = useRef<string | null>(null);
  selectedIdRef.current = selected?.id ?? null;
  const boardIdRef = useRef<string | null>(null);
  boardIdRef.current = boardId;

  const refresh = useCallback(async () => {
    try {
      const [b, p, a, m, ss] = await Promise.all([
        api.board(boardIdRef.current ?? undefined),
        api.projects(),
        api.agents(),
        api.members(),
        api.sessions(),
      ]);
      setBoard(b);
      setProjects(p);
      setAgents(a);
      setMembers(m);
      setSessions(ss);
      setApprovals(await api.approvals()); // 全量：pending 待办 + 最近已处理（审批闭环呈现）
      setNotifications(await api.notifications("u-owner", 0));
      if (selectedIdRef.current) {
        setSelected(await api.card(selectedIdRef.current));
      }
    } catch { /* core 未启动时静默 */ }
  }, []);

  useEffect(() => {
    // 长轮询实时推送（F-401）：有新事件立即返回，否则服务端挂起 25s。
    // 注意先完成首次 refresh 再开始长轮询：长轮询会占用一个浏览器连接 25s，
    // 与首屏 5 个并发请求争抢同域连接上限（尤其反复刷新、旧 socket 未释放时），
    // 曾导致首屏"正在连接"卡数十秒。
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let wasDown = false; // core 断连过：恢复后必须补一次 refresh，否则首屏数据永远为空
    (async function poll(since: number) {
      await refresh();
      while (alive) {
        try {
          const r = await fetch(`${API_BASE}/api/v1/events?since=${since}`);
          const d = await r.json();
          since = d.last_seq ?? since;
          if (wasDown) { wasDown = false; refresh(); }
          if (d.events?.length) {
            clearTimeout(timer);
            timer = setTimeout(refresh, 100); // 防抖合并密集事件
          }
        } catch {
          wasDown = true;
          await new Promise((r) => setTimeout(r, 3000)); // core 未启动时退避重试
        }
      }
    })(0);
    return () => { alive = false; clearTimeout(timer); };
  }, [refresh]);

  const showError = (e: unknown) => {
    const err = e as { data?: { error?: string }; message?: string };
    setToast(err?.data?.error ?? err?.message ?? "操作失败");
    setToastKind("error");
    setTimeout(() => setToast(null), 4000);
  };
  const info = (msg: string) => { setToast(msg); setToastKind("info"); setTimeout(() => setToast(null), 3000); };

  const openCard = async (id: string) => {
    try { setSelected(await api.card(id)); } catch (e) { showError(e); }
  };

  const createCard = async () => {
    if (!newTitle.trim()) return;
    try {
      const c = await api.createCard(newTitle.trim(), "", board?.board_id);
      setNewTitle("");
      setSelected(c); // 建卡即打开抽屉：引导指派/补充描述，而不是让卡片静默消失在列里
    } catch (e) { showError(e); }
  };

  const switchBoard = (id: string) => {
    setSelected(null);
    setBoardId(id);
    // boardIdRef 同步后由 refresh 拉取新看板
    boardIdRef.current = id;
    refresh();
  };

  const createProject = async () => {
    if (!newProject.trim()) return;
    try {
      const p = await api.createProject(newProject.trim(), "", newTpl) as Project & { board?: { id: string } };
      setNewProject("");
      setProjFormOpen(false);
      await refresh();
      const bid = (p as { board?: { id: string } }).board?.id ?? p.boards?.[0]?.id;
      if (bid) switchBoard(bid);
    } catch (e) { showError(e); }
  };

  const renameProject = async (pid: string) => {
    if (!editName.trim()) return;
    try {
      await api.renameProject(pid, editName.trim());
      setEditingProj(null);
      await refresh();
    } catch (e) { showError(e); }
  };

  const deleteProject = async (pid: string) => {
    try {
      await api.deleteProject(pid);
      setDelArming(null);
      // 删的若是当前看板所属项目，回到默认看板
      if (currentProject?.id === pid) {
        boardIdRef.current = null;
        setBoardId(null);
        setSelected(null);
      }
      await refresh();
    } catch (e) { showError(e); }
  };

  const currentBoardId = board?.board_id ?? boardId;
  const currentProject = projects.find((p) => p.boards.some((b) => b.id === currentBoardId));
  const currentBoard = currentProject?.boards.find((b) => b.id === currentBoardId);

  const dropCard = async (card: CardSummary, list: ListWithCards) => {
    try {
      const r = await api.move(card.id, list.id, card.rev, card.claim ? card.claim.holder_id : "u-owner");
      if (r.approval_pending) info("已提交审批，等待人类批准");
    } catch (e) { showError(e); }
  };

  const decide = async (id: string, decision: "approved" | "rejected") => {
    try { await api.decide(id, decision); refresh(); }
    catch (e) { showError(e); }
  };

  const pendingCount = approvals.filter((a) => a.status === "pending").length;
  const unreadCount = notifications.filter((n) => n.seq > lastRead).length;
  const activeAgents = agents.filter((a) => !a.revoked);
  const onlineCount = activeAgents.filter((a) => a.online).length;

  const toggleNotifs = () => {
    const next = !showNotifs;
    setShowNotifs(next);
    if (next && notifications.length > 0) {
      const maxSeq = Math.max(...notifications.map((n) => n.seq));
      setLastRead(maxSeq);
      localStorage.setItem("baton.last_read_seq", String(maxSeq));
    }
  };

  return (
    <div className="app">
      <header className="topbar">
        {/* 左：我是谁、我在哪 */}
        <div className="brand">Baton<em>●</em></div>
        <span className="crumb">
          {currentProject ? `${currentProject.name} / ${currentBoard?.name ?? ""}` : "…"}
        </span>
        <div className="spacer" />
        {/* 右：次级面板开关 → 环境状态 → 主行动线（建卡）。
            Agent 在场状态压成一个可点汇总芯片，点击打开 Agent 面板，不再平铺占栏 */}
        <button className="btn" onClick={() => setShowApprovals(!showApprovals)}>
          🔔 审批{pendingCount > 0 && <span className="pill">{pendingCount}</span>}
        </button>
        <button className="btn" onClick={toggleNotifs}>
          📨 通知{unreadCount > 0 && <span className="pill">{unreadCount}</span>}
        </button>
        <button className="btn" onClick={() => setShowAgents(!showAgents)}>
          🤖 Agent
        </button>
        <button
          className={`presence-chip ${onlineCount > 0 ? "online" : ""}`}
          title={activeAgents.map((a) => `${a.online ? "🟢" : "⚪"} ${a.name}（${a.role}）${a.holding_cards.length ? ` · 持 ${a.holding_cards.length} 卡` : ""}`).join("\n")}
          onClick={() => setShowAgents(true)}
        >
          {onlineCount > 0 ? `🟢 ${onlineCount} 在岗` : "⚪ 无 Agent 在岗"}
          <span className="muted">· {activeAgents.length} 在编</span>
        </button>
        <input
          value={newTitle}
          onChange={(e) => setNewTitle(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && !e.nativeEvent.isComposing && createCard()}
          placeholder="新卡片标题，回车创建 →"
        />
        <button className="btn btn-primary" onClick={createCard}>＋ 建卡</button>
      </header>

      {toast && <div className={`toast ${toastKind === "error" ? "error" : ""}`}>{toast}</div>}

      {showApprovals && (
        <>
          <div className="panel-overlay" onClick={() => setShowApprovals(false)} />
          <section className="panel">
          <div className="panel-head">
            <h3>审批中心</h3>
            <button className="close" onClick={() => setShowApprovals(false)}>×</button>
          </div>
          {pendingCount === 0 && <div className="empty-hint">没有待审批事项。Agent 把卡片移入需审批的列时，会出现在这里。</div>}
          {approvals.filter((a) => a.status === "pending").map((a) => (
            <div key={a.id} className="panel-row">
              <span className="a-title">{a.card_title}</span>
              <span className="muted">→ {LIST_NAMES[a.list_id] ?? a.list_id} · 由 {memberName(a.requested_by)} 申请</span>
              <div className="spacer" />
              <button className="btn btn-success btn-sm" onClick={() => decide(a.id, "approved")}>✓ 通过</button>
              <button className="btn btn-danger btn-sm" onClick={() => decide(a.id, "rejected")}>✗ 打回</button>
            </div>
          ))}
          {/* 审批闭环：让申请人（Agent）和人类都能看到审批结果的历史 */}
          {approvals.filter((a) => a.status !== "pending").length > 0 && (
            <>
              <h4>最近已处理</h4>
              {approvals.filter((a) => a.status !== "pending").slice(0, 5).map((a) => (
                <div key={a.id} className="panel-row">
                  <span className={`tag ${a.status}`}>{a.status === "approved" ? "✅ 已通过" : "❌ 已打回"}</span>
                  <span className="a-title">{a.card_title}</span>
                  <span className="muted">→ {LIST_NAMES[a.list_id] ?? a.list_id} · {memberName(a.requested_by)}</span>
                  <div className="spacer" />
                  <span className="muted">{a.created_at.slice(0, 16).replace("T", " ")}</span>
                </div>
              ))}
            </>
          )}
          </section>
        </>
      )}

      {showNotifs && (
        <>
          <div className="panel-overlay" onClick={() => setShowNotifs(false)} />
          <section className="panel">
          <div className="panel-head">
            <h3>通知中心</h3>
            <button className="close" onClick={() => setShowNotifs(false)}>×</button>
          </div>
          {notifications.length === 0 && <div className="empty-hint">暂无通知</div>}
          {notifications.slice(0, 30).map((n) => (
            <div key={n.seq} className={`panel-row ${n.seq > lastRead ? "unread" : ""}`}>
              <span className="tag">{n.kind}</span>
              <span className="muted">{memberName(n.actor_id)}</span>
              {n.card_id && (
                <a href="#" onClick={(e) => { e.preventDefault(); openCard(n.card_id!); }}>
                  {n.card_id}
                </a>
              )}
              <div className="spacer" />
              <span className="muted">{n.at}</span>
            </div>
          ))}
          </section>
        </>
      )}

      {showAgents && (
        <>
          <div className="panel-overlay" onClick={() => setShowAgents(false)} />
          <AgentPanel agents={agents} sessions={sessions} onChanged={refresh} onError={showError}
            onClose={() => setShowAgents(false)} />
        </>
      )}

      {showInstall && <InstallPanel onClose={() => setShowInstall(false)} />}

      <div className="body">
        <aside className="sidebar">
          <div className="sidebar-scroll">
            <h3 className="side-title">项目</h3>
          {/* 扁平看板列表：一行一个看板（看板名 + 项目名），悬浮出现项目操作。
              大多数项目只有一个看板，不再为层级浪费一行。 */}
          {projects.flatMap((p) => p.boards.map((b) => ({ p, b }))).map(({ p, b }) => (
            <div key={b.id} className="board-row">
              {editingProj === p.id ? (
                <div className="board-rename">
                  <input
                    autoFocus
                    value={editName}
                    onChange={(e) => setEditName(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && !e.nativeEvent.isComposing) renameProject(p.id);
                      if (e.key === "Escape") setEditingProj(null);
                    }}
                  />
                  <button className="btn btn-primary btn-sm" onClick={() => renameProject(p.id)}>存</button>
                  <button className="btn btn-sm" onClick={() => setEditingProj(null)}>×</button>
                </div>
              ) : (
                <>
                  <button
                    className={`board-link ${b.id === currentBoardId ? "active" : ""}`}
                    onClick={() => switchBoard(b.id)}
                  >
                    <span className="board-name">{b.name}</span>
                    <span className="board-proj">{p.name}</span>
                  </button>
                  <span className="row-actions">
                    <button title="重命名项目" onClick={() => { setEditingProj(p.id); setEditName(p.name); }}>✎</button>
                    {delArming === p.id ? (
                      <button className="arm" title="再次点击确认删除：该项目下的看板与卡片将一并删除"
                        onClick={() => deleteProject(p.id)}>确认删除？</button>
                    ) : (
                      <button title="删除项目" onClick={() => {
                        setDelArming(p.id);
                        setTimeout(() => setDelArming((cur) => (cur === p.id ? null : cur)), 3000);
                      }}>×</button>
                    )}
                  </span>
                </>
              )}
            </div>
          ))}
          </div>
          {/* setup 类动作钉在侧栏底部：新建项目 / 接入 Agent */}
          <div className="sidebar-foot">
            {projFormOpen ? (
              <div className="form-row sidebar-new">
                <input
                  autoFocus
                  value={newProject}
                  onChange={(e) => setNewProject(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && !e.nativeEvent.isComposing && createProject()}
                  placeholder="新项目名"
                />
                <select value={newTpl} onChange={(e) => setNewTpl(e.target.value)} title="看板模板">
                  <option value="software">软件开发</option>
                  <option value="content">内容生产</option>
                  <option value="gtd">通用 GTD</option>
                </select>
                <div className="actions">
                  <button className="btn btn-primary btn-sm" onClick={createProject}>创建</button>
                  <button className="btn btn-sm" onClick={() => setProjFormOpen(false)}>取消</button>
                </div>
              </div>
            ) : (
              <button className="btn-link sidebar-new-link" onClick={() => setProjFormOpen(true)}>
                ＋ 新建项目
              </button>
            )}
            <button className="btn-link sidebar-new-link" onClick={() => setShowInstall(true)}>
              ⇄ 接入 Agent
            </button>
          </div>
        </aside>

        <div className="board-wrap">
          {/* 情境引导：没有任何 Agent 在岗时才出现，可关闭且记住选择 */}
          {board && onlineCount === 0 && !installDismissed && (
            <div className="onboard-banner">
              <span>⚡ 还没有 Agent 在岗 —— 接入一个 Agent，让它开始认领卡片干活。</span>
              <button className="btn btn-primary btn-sm" onClick={() => setShowInstall(true)}>查看接入指引</button>
              <button className="close" title="知道了，不再提示" onClick={() => {
                setInstallDismissed(true);
                localStorage.setItem("baton.install_dismissed", "1");
              }}>×</button>
            </div>
          )}
          <main className="board">
            {board?.lists.map((l) => (
              <Column key={l.id} list={l} onOpen={openCard} onDrop={dropCard}
                dragging={dragging}
                onDragCardStart={setDragging}
                onDragCardEnd={() => setDragging(null)} />
            ))}
            {!board && <div className="loading">正在连接 core server（127.0.0.1:7700）…</div>}
          </main>
        </div>
      </div>

      {selected && (
        <CardDrawer
          key={selected.id}
          card={selected}
          board={board}
          members={members}
          onClose={() => setSelected(null)}
          onError={showError}
          onInfo={info}
          onDone={async (next) => { if (next) setSelected(next); }}
          onOpenCard={openCard}
        />
      )}
    </div>
  );
}

function Column({
  list, onOpen, onDrop, dragging, onDragCardStart, onDragCardEnd,
}: {
  list: ListWithCards;
  onOpen: (id: string) => void;
  onDrop: (card: CardSummary, list: ListWithCards) => void;
  dragging: CardSummary | null;
  onDragCardStart: (c: CardSummary) => void;
  onDragCardEnd: () => void;
}) {
  const [dragOver, setDragOver] = useState(false);
  const overWip = list.wip_limit != null && list.cards.length > list.wip_limit;
  // 拖拽预告：落点列的策略会不会拦截这次移动（不用等松手后吃 toast）
  const incoming = dragging && !list.cards.some((c) => c.id === dragging.id) ? dragging : null;
  const willReject = !!incoming && !!incoming.claim
    && list.policy.require_progress_summary === true && incoming.progress_percent == null;
  const willApprove = !!incoming && !willReject && !!list.policy.require_approval;
  return (
    <section
      className={`column ${overWip ? "over-wip" : ""} ${dragOver ? "drag-over" : ""} ${dragOver && willReject ? "drag-reject" : ""}`}
      onDragOver={(e) => { e.preventDefault(); setDragOver(true); }}
      onDragLeave={() => setDragOver(false)}
      onDrop={(e) => {
        e.preventDefault(); setDragOver(false);
        const raw = e.dataTransfer.getData("application/x-baton-card");
        if (!raw) return;
        const card = JSON.parse(raw) as CardSummary;
        if (card.id && !list.cards.some((c) => c.id === card.id)) onDrop(card, list);
      }}
    >
      <div className="col-head">
        <span className="col-dot" style={{ background: list.policy.is_done ? "var(--success)" : list.policy.require_approval ? "var(--warn)" : "var(--human)" }} />
        <span className="col-name">{list.name}</span>
        {list.policy.require_approval && <span className="col-hint" title="该列需人类审批才能进入">🛂 需审批</span>}
        <span className="count" title={list.wip_limit != null ? `在制品上限 ${list.wip_limit}` : undefined}>
          {list.cards.length}{list.wip_limit != null && ` / ${list.wip_limit}`}
        </span>
      </div>
      {dragOver && willReject && <div className="drag-hint reject">✋ 该列要求进度摘要，这次移动会被拒绝</div>}
      {dragOver && willApprove && <div className="drag-hint approve">🛂 将提交人工审批，不会直接移动</div>}
      <div className="cards">
        {list.cards.map((c) => (
          <CardChip key={c.id} card={c} onOpen={onOpen}
            onDragCardStart={onDragCardStart} onDragCardEnd={onDragCardEnd} />
        ))}
        {list.cards.length === 0 && <div className="col-empty">把卡片拖到这里，或用右上角「＋ 建卡」</div>}
      </div>
    </section>
  );
}

function CardChip({ card, onOpen, onDragCardStart, onDragCardEnd }: {
  card: CardSummary;
  onOpen: (id: string) => void;
  onDragCardStart: (c: CardSummary) => void;
  onDragCardEnd: () => void;
}) {
  return (
    <div
      className={`card p-${card.priority}${card.claim ? " agent-held" : ""}`}
      draggable
      title="点击查看详情，拖拽可移列"
      onDragStart={(e) => {
        e.dataTransfer.setData("application/x-baton-card", JSON.stringify(card));
        onDragCardStart(card);
      }}
      onDragEnd={onDragCardEnd}
      onClick={() => onOpen(card.id)}
    >
      {/* 票据美学：等宽卡号 + rev，虚线撕缝线分隔（DESIGN.md） */}
      <div className="card-id"><span>bat-{card.id.slice(-8, -4)}</span><span>rev {card.rev}</span></div>
      <div className="card-title">{card.title}</div>
      <div className="card-meta">
        {card.parent_id && <span className="mini-badge">↳ 子任务</span>}
        {(card.participants?.length ?? 0) > 0 && (
          <span className="mini-badge claimed">🤝 {card.participants!.map(memberName).join("、")}</span>
        )}
        {card.progress_percent != null && <span className="mini-badge progress">◔ {card.progress_percent}%</span>}
        {card.claim && <span className="mini-badge claimed">🟢 {memberName(card.claim.holder_id)}</span>}
        {!card.claim && card.assignee_id && <span className="mini-badge">👤 {memberName(card.assignee_id)}</span>}
        {!card.claim && !card.assignee_id && <span className="mini-badge pool">可抢</span>}
        {card.open_threads > 0 && <span className="mini-badge">💬 {card.open_threads}</span>}
        {card.handoff_state === "ready" && <span className="stamp">已移交</span>}
        {card.handoff_state && card.handoff_state !== "none" && card.handoff_state !== "ready" && (
          <span className="mini-badge handoff">⇄ 移交中</span>
        )}
      </div>
    </div>
  );
}

// 评论树：按 reply_to 把回复嵌套在被回复评论下方（顶层按时间排序），
// 让讨论串的前因后果在视觉上连贯。父评论缺失的孤儿回复按顶层处理。
function CommentTree({ comments, onReply }: { comments: Comment[]; onReply: (c: Comment) => void }) {
  const childrenOf = (id: string) => comments.filter((c) => c.reply_to === id);
  const isTop = (c: Comment) => !c.reply_to || !comments.some((p) => p.id === c.reply_to);
  const renderNode = (c: Comment, depth: number): JSX.Element => (
    <div key={c.id}>
      <div className={`comment kind-${c.kind}${depth > 0 ? " nested" : ""}`}>
        <span className="author">{memberName(c.author_id)}</span>
        <span className="kind">{c.kind}</span>
        {c.kind === "chat" && (
          <button className="reply-btn" title="回复这条评论" onClick={() => onReply(c)}>回复</button>
        )}
        <div>{c.body}</div>
      </div>
      {childrenOf(c.id).map((ch) => renderNode(ch, depth + 1))}
    </div>
  );
  return <>{comments.filter(isTop).map((c) => renderNode(c, 0))}</>;
}

// 话题内的评论输入框：回复时显示目标摘录（只在目标所在话题下出现）
function ThreadInput({ replyTo, onCancelReply, onSend }: {
  replyTo: Comment | null;
  onCancelReply: () => void;
  onSend: (text: string) => void;
}) {
  const [text, setText] = useState("");
  return (
    <div className="comment-box">
      {replyTo && (
        <div className="reply-chip">
          回复 {memberName(replyTo.author_id)}：{replyTo.body.slice(0, 30)}
          <button title="取消回复" onClick={onCancelReply}>×</button>
        </div>
      )}
      <input
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          // IME 组合中（中文输入法选词/上屏）的 Enter 不触发发送
          if (e.key === "Enter" && !e.nativeEvent.isComposing && text.trim()) {
            onSend(text.trim());
            setText("");
          }
        }}
        placeholder={replyTo ? "回复，回车发送" : "评论，回车发送"}
      />
    </div>
  );
}

function CardDrawer({
  card, board, members, onClose, onError, onInfo, onDone, onOpenCard,
}: {
  card: CardDetail;
  board: BoardState | null;
  members: Member[];
  onClose: () => void;
  onError: (e: unknown) => void;
  onInfo: (msg: string) => void;
  onDone: (next: CardDetail | null) => Promise<void>;
  onOpenCard: (id: string) => void; // 打开另一张卡（父/子跳转）
}) {
  const [tab, setTab] = useState<Tab>("讨论");
  const [replyTo, setReplyTo] = useState<Comment | null>(null); // 正在回复的评论
  const [topicOpen, setTopicOpen] = useState(false); // 新建话题输入框
  const [newTopic, setNewTopic] = useState("");
  const [childOpen, setChildOpen] = useState(false); // 新建子任务输入框
  const [childTitle, setChildTitle] = useState("");
  const claimed = !!card.claim;
  const lists = board?.lists ?? [];
  const idx = lists.findIndex((l) => l.id === card.list_id);
  const prev = lists[idx - 1];
  const next = lists[idx + 1];
  const handoffState = card.ext.handoff?.state ?? "none";
  // 回复目标所在的话题 id（回复输入框只出现在该话题下）
  const replyThreadId = replyTo
    ? card.threads.find((t) => t.comments.some((c) => c.id === replyTo.id))?.id ?? null
    : null;

  const run = async (fn: () => Promise<CardDetail | { approval_pending?: string } | unknown>) => {
    try {
      const r = (await fn()) as CardDetail & { approval_pending?: string };
      if (r?.approval_pending) onInfo("已提交审批，等待人类批准");
      else if (r?.id) await onDone(r);
    } catch (e) { onError(e); }
  };

  const moveActor = claimed ? card.claim!.holder_id : "u-owner";

  return (
    <>
      <div className="backdrop" onClick={onClose} />
      <aside className="drawer">
      <div className="drawer-head">
        <h2>{card.title}</h2>
        <button className="close" title="关闭（或点击遮罩）" onClick={onClose}>×</button>
      </div>
      <div className="meta-line">
        <span className="meta-chip">rev {card.rev}</span>
        <span className="meta-chip mono" title="卡片 id（CLI/MCP 操作用）">{card.id}</span>
        <span className="meta-chip">创建者 {memberName(card.created_by)}</span>
      </div>
      {/* 状态横幅：不用读细节就能回答"这卡现在什么状态、下一步是什么" */}
      <div className={`status-banner ${claimed ? "agent" : ""}`}>
        {claimed && <>🟢 <b>{memberName(card.claim!.holder_id)}</b> 正在处理 · 租约至 {card.claim!.lease_until}</>}
        {!claimed && card.assignee_id && <>👤 已指派给 <b>{memberName(card.assignee_id)}</b>，等待认领</>}
        {!claimed && !card.assignee_id && <>◌ 在抢单池中 —— 等待 Agent 认领，或用下方「指派」分配</>}
        {card.participants.length > 0 && (
          <div className="status-sub">🤝 协同中：{card.participants.map(memberName).join("、")}（可评论/汇报/移列，主责不变）</div>
        )}
        {handoffState === "preparing" && <div className="status-sub">⚑ 正在准备移交</div>}
        {handoffState === "ready" && <div className="status-sub">⚑ 移交已就绪，等待接手</div>}
      </div>
      {card.ext.progress && (
        <div className="progress-line">
          <div className="bar"><div style={{ width: `${card.ext.progress.percent}%` }} /></div>
          <span>{card.ext.progress.percent}% · {card.ext.progress.summary}</span>
        </div>
      )}
      {/* 依赖（F-106/305）：blocked_by 未完成的卡片禁入 Done 列 */}
      {card.deps.length > 0 && (
        <div className="deps-line">
          {card.deps.map((d) => (
            <span key={`${d.relation}-${d.other_id}`}
              className={`dep ${d.relation === "blocked_by" ? (d.other_done ? "ok" : "blocked") : ""}`}
              title={`${d.other_id} · ${d.other_list_id}`}>
              {d.relation === "blocked_by" ? (d.other_done ? "✅ 依赖" : "⛔ 阻塞于")
                : d.relation === "blocks" ? "→ 阻塞" : "↔"} {d.other_title}
              <button className="dep-x" title="移除依赖"
                onClick={() => run(() => api.removeDep(card.id, d.other_id, d.relation))}>×</button>
            </span>
          ))}
        </div>
      )}
      <DepsAdder card={card} board={board} run={run} />
      {card.description && <p className="desc">{card.description}</p>}

      {/* 父子任务（F-107）：任务跑着跑着才拆得清——随时把一部分拆出去 */}
      <div className="children-section">
        {card.parent && (
          <div className="parent-line">
            ↩ 父任务：<a href="#" onClick={(e) => { e.preventDefault(); onOpenCard(card.parent!.id); }}>{card.parent.title}</a>
          </div>
        )}
        {(card.children.length > 0 || childOpen) && (
          <>
            <h4>子任务（{card.children.filter((c) => c.done).length}/{card.children.length} 完成）</h4>
            {card.children.map((ch) => (
              <div key={ch.id} className="child-row" onClick={() => onOpenCard(ch.id)}>
                <span>{ch.done ? "✅" : "◌"}</span>
                <span className="child-title">{ch.title}</span>
                {ch.progress_percent != null && <span className="mini-badge progress">◔ {ch.progress_percent}%</span>}
                <span className="muted">{LIST_NAMES[ch.list_id] ?? ch.list_id}</span>
              </div>
            ))}
          </>
        )}
        {childOpen ? (
          <div className="form-row">
            <input
              autoFocus
              value={childTitle}
              onChange={(e) => setChildTitle(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.nativeEvent.isComposing && childTitle.trim()) {
                  run(async () => {
                    await api.createCard(childTitle.trim(), "", undefined, card.id);
                    return api.card(card.id);
                  });
                  setChildTitle("");
                  setChildOpen(false);
                }
                if (e.key === "Escape") setChildOpen(false);
              }}
              placeholder="子任务标题，回车创建（进入看板第一列）"
            />
          </div>
        ) : (
          <button className="btn-link" onClick={() => setChildOpen(true)}>＋ 拆出子任务</button>
        )}
      </div>

      <div className="action-group">
        <div className="action-group-title">操作</div>
        <div className="actions">
          {/* 指派（F-105/303）：空 = 抢单池。指派是动作，归操作区，不混在元信息里 */}
          <span className="assign-wrap">
            <span className="muted">指派</span>
            <select
              className="assign-select"
              value={card.assignee_id ?? ""}
              onChange={(e) => run(() => api.assign(card.id, e.target.value || null))}
              title="指派给成员；空 = 抢单池"
            >
              <option value="">抢单池</option>
              {members.filter((m) => !m.revoked).map((m) => (
                <option key={m.id} value={m.id}>{m.kind === "agent" ? "🤖 " : ""}{m.name}</option>
              ))}
            </select>
          </span>
          {/* GUI 只提供人的动作：移列、协调（收回租约、指派）。
              认领（claim）是 Agent 的自主行为，走 MCP/CLI card_claim，不由人代办。 */}
          {prev && <button className="btn" onClick={() => run(() => api.move(card.id, prev.id, card.rev, moveActor))}>← {prev.name}</button>}
          {next && <button className="btn btn-primary" onClick={() => run(() => api.move(card.id, next.id, card.rev, moveActor))}>→ {next.name}</button>}
          {claimed && (
            <button className="btn btn-danger" title="协调动作：从卡住的 Agent 手里强制收回租约，卡片回到可认领状态，可重新指派（F-405）"
              onClick={() => run(() => api.takeover(card.id))}>
              ↩ 收回租约
            </button>
          )}
        </div>
      </div>

      <nav className="tabs">
        {(["讨论", "需求", "Git", "现场", "移交", "产物"] as Tab[]).map((t) => (
          <button key={t} className={tab === t ? "tab active" : "tab"} onClick={() => setTab(t)}>{t}</button>
        ))}
      </nav>

      {tab === "讨论" && (
        <>
          {/* 话题索引：一目了然有多少话题，点击滚动定位 */}
          <div className="thread-index">
            {card.threads.map((t) => (
              <button key={t.id} className="thread-chip" title="点击定位到该话题"
                onClick={() => document.getElementById(`thread-${t.id}`)
                  ?.scrollIntoView({ behavior: "smooth", block: "start" })}>
                {t.title ?? "话题"} · {t.comments.length}{t.status !== "open" ? `（${t.status}）` : ""}
              </button>
            ))}
            <button className="thread-chip new" onClick={() => setTopicOpen(!topicOpen)}>＋ 新话题</button>
          </div>
          {topicOpen && (
            <div className="form-row">
              <input autoFocus value={newTopic} onChange={(e) => setNewTopic(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.nativeEvent.isComposing && newTopic.trim()) {
                    run(() => api.createThread(card.id, newTopic.trim()));
                    setNewTopic("");
                    setTopicOpen(false);
                  }
                }}
                placeholder="话题名，回车创建" />
            </div>
          )}
          {card.threads.map((t) => (
            <div key={t.id} className="thread" id={`thread-${t.id}`}>
              <div className="thread-title"># {t.title ?? "话题"} · {t.status} · {t.comments.length} 条</div>
              <CommentTree comments={t.comments} onReply={setReplyTo} />
              <ThreadInput
                replyTo={t.id === replyThreadId ? replyTo : null}
                onCancelReply={() => setReplyTo(null)}
                onSend={(text) => {
                  const rt = t.id === replyThreadId ? replyTo : null;
                  run(() => api.comment(card.id, text, "u-owner", "chat", rt?.id, rt ? undefined : t.id));
                  if (rt) setReplyTo(null);
                }}
              />
            </div>
          ))}
        </>
      )}

      {tab === "需求" && <LinksTab card={card} run={run} />}
      {tab === "Git" && <GitTab card={card} run={run} />}
      {tab === "现场" && <WorksiteTab card={card} run={run} />}
      {tab === "移交" && (
        <HandoffTab card={card} state={handoffState} run={run} />
      )}
      {tab === "产物" && <ArtifactsTab card={card} run={run} />}
      </aside>
    </>
  );
}

function LinksTab({ card, run }: { card: CardDetail; run: (fn: () => Promise<unknown>) => void }) {
  const [title, setTitle] = useState("");
  const [url, setUrl] = useState("");
  const [system, setSystem] = useState("jira");
  return (
    <div>
      <div className="tab-hint">关联需求来源与文档（Jira / 链接 / 本地文件），让干活的 Agent 有据可依。</div>
      <h4>需求来源</h4>
      {card.links.filter((l) => l.category === "source").map((l) => (
        <div key={l.id} className="kv-row">
          <span className="tag">{l.system}</span>
          {l.key && <span className="mono">{l.key}</span>}
          <a href={l.url ?? "#"} target="_blank" rel="noreferrer">{l.title || l.url}</a>
          <span className="muted">{l.relation === "origin" ? "来源" : "关联"}</span>
        </div>
      ))}
      <h4>需求文档</h4>
      {card.links.filter((l) => l.category === "doc").map((l) => (
        <div key={l.id} className="kv-row">
          <span className="tag">{l.kind}</span>
          {l.url ? <a href={l.url} target="_blank" rel="noreferrer">{l.title}</a>
                 : <span className="mono">{l.path ?? l.title}</span>}
        </div>
      ))}
      {card.links.length === 0 && <div className="muted">暂无链接</div>}
      <div className="form-row">
        <select value={system} onChange={(e) => setSystem(e.target.value)}>
          <option value="jira">Jira</option><option value="meego">MeeGo</option>
          <option value="url">URL</option><option value="file">本地文件</option>
        </select>
        <input value={title} onChange={(e) => setTitle(e.target.value)} placeholder="标题 / PROJ-123" />
        <input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://… 或 /path" />
        <button className="btn btn-sm" onClick={() => run(() => api.addLink(card.id, {
          category: "source", system, title, url,
          relation: "origin", kind: system === "file" ? "local_file" : "url",
        }))}>＋ 添加</button>
      </div>
    </div>
  );
}

function GitTab({ card, run }: { card: CardDetail; run: (fn: () => Promise<unknown>) => void }) {
  const [path, setPath] = useState("");
  const [branch, setBranch] = useState("");
  const repos = card.ext.git?.repos ?? [];
  return (
    <div>
      <div className="tab-hint">关联代码仓库并探测真实 git 状态（暂存/未跟踪/领先落后），人在这里监督 Agent 的工作区是否干净。</div>
      {repos.map((r, i) => (
        <div key={i} className="repo">
          <div className="mono">{r.repo_path}</div>
          <div className="muted">分支 {r.branch}{r.declared.base_branch && ` ← ${r.declared.base_branch}`}</div>
          {r.observed && (
            r.observed.error
              ? <div className="error-text">探测失败：{r.observed.error}</div>
              : <div className="git-status">
                  {r.observed.clean ? "✅ 干净" : (
                    <>
                      {r.observed.staged ? <span>●{r.observed.staged} staged</span> : null}
                      {r.observed.unstaged ? <span>{r.observed.unstaged} 未暂存</span> : null}
                      {r.observed.untracked ? <span>{r.observed.untracked} 未跟踪</span> : null}
                    </>
                  )}
                  {r.observed.ahead ? <span>↑{r.observed.ahead}</span> : null}
                  {r.observed.behind ? <span>↓{r.observed.behind}</span> : null}
                  <span className="muted">截至 {r.observed.snapshot_at}</span>
                </div>
          )}
        </div>
      ))}
      {repos.length === 0 && <div className="muted">未关联仓库</div>}
      <div className="form-row">
        <input value={path} onChange={(e) => setPath(e.target.value)} placeholder="仓库绝对路径" />
        <input value={branch} onChange={(e) => setBranch(e.target.value)} placeholder="分支" />
        <button className="btn btn-sm" onClick={() => run(() => api.gitAttach(card.id, path, branch))}>关联</button>
      </div>
      {repos.length > 0 && (
        <button className="btn btn-sm" onClick={() => run(() => api.gitRefresh(card.id))}>🔄 探测 git 状态</button>
      )}
    </div>
  );
}

function WorksiteTab({ card, run }: { card: CardDetail; run: (fn: () => Promise<unknown>) => void }) {
  const [path, setPath] = useState("");
  const [branch, setBranch] = useState("");
  const [purpose, setPurpose] = useState("");
  const main = card.work_nodes.filter((n) => n.kind === "main");
  const trees = card.work_nodes.filter((n) => n.kind === "worktree");
  return (
    <div>
      <div className="tab-hint">登记这张卡的工作现场：主工作目录和 worktree——Agent 在哪里干活、干活的上下文在哪。</div>
      <h4>主工作目录</h4>
      {main.map((n) => (
        <div key={n.id} className="node main-node">
          <span className="mono">{n.path}</span>
          <span className="tag">{n.branch}</span>
        </div>
      ))}
      {main.length === 0 && <div className="muted">未登记</div>}
      <h4>Worktrees（{trees.length}）</h4>
      {trees.map((n) => (
        <div key={n.id} className="node">
          <span className="mono">{n.path}</span>
          <span className="tag">{n.branch}</span>
          {n.purpose && <span className="muted">{n.purpose}</span>}
          {n.owner_id && <span>👤 {memberName(n.owner_id)}</span>}
          {n.bound_card_id && <span className="muted">→ {n.bound_card_id}</span>}
        </div>
      ))}
      <div className="form-row">
        <input value={path} onChange={(e) => setPath(e.target.value)} placeholder="路径" />
        <input value={branch} onChange={(e) => setBranch(e.target.value)} placeholder="分支" />
        <input value={purpose} onChange={(e) => setPurpose(e.target.value)} placeholder="用途" />
        <button className="btn btn-sm" onClick={() => run(() => api.worksiteAdd(card.id, {
          kind: main.length === 0 ? "main" : "worktree", path, branch, purpose, owner: "a-code",
        }))}>＋ 登记</button>
      </div>
    </div>
  );
}

// 复制块：一段可复制的安装/接入指引 + 复制按钮（带反馈）
function CopyBlock({ label, text }: { label: string; text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="copy-block">
      <div className="copy-head">
        <span className="copy-label">{label}</span>
        <button className="btn btn-sm" onClick={() => {
          navigator.clipboard?.writeText(text);
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        }}>{copied ? "✓ 已复制" : "复制"}</button>
      </div>
      <pre className="copy-text">{text}</pre>
    </div>
  );
}

// 接入指引面板：onboarding 意图，独立于 Agent 管理（在编生命周期）面板
function InstallPanel({ onClose }: { onClose: () => void }) {
  const [installInfo, setInstallInfo] = useState<{ mcp_bin: string } | null>(null);
  useEffect(() => { api.installInfo().then(setInstallInfo).catch(() => {}); }, []);
  return (
    <>
      <div className="panel-overlay" onClick={onClose} />
      <section className="panel">
        <div className="panel-head">
          <h3>接入新 Agent</h3>
          <button className="close" onClick={onClose}>×</button>
        </div>
        <div className="tab-hint">
          三种方式任选：Claude Code 一键命令；通用 MCP 配置；或把整段指引直接贴给你的 Agent，让它自己完成安装。
          接入后 Agent 即可进板认领任务。
        </div>
        {installInfo && (
          <>
            <CopyBlock label="Claude Code（命令行一键）"
              text={`claude mcp add baton -- ${installInfo.mcp_bin}`} />
            <CopyBlock label="通用 MCP 配置（Cursor / Claude Desktop / Kimi Code 等）"
              text={JSON.stringify({ mcpServers: { baton: { command: installInfo.mcp_bin } } }, null, 2)} />
            <CopyBlock label="粘贴给你的 Agent（自包含指引）"
              text={`请把 Baton 看板接入为你的 MCP server，完成后你就能在任务看板上认领和处理工作：

1. MCP server 二进制：${installInfo.mcp_bin}（stdio，newline-delimited JSON-RPC）
2. 安装（任选其一）：
   - Claude Code：claude mcp add baton -- ${installInfo.mcp_bin}
   - 其他 Agent：在 MCP 配置中加入 {"mcpServers":{"baton":{"command":"${installInfo.mcp_bin}"}}}
3. 验证：调用 board_get 确认连通。进程启动会自动 session_start（进板），退出自动 session_end。
4. 工作方式：card_claim 认领卡片 → 干活 → progress_update 汇报进展 → card_move 移列。
   注意：移动需携带当前 rev（乐观锁）；进入"进行中"列前必须先 progress_update 上报进度摘要。
5. 如果你是全新 Agent：POST http://127.0.0.1:7700/api/v1/agents 可自注册并领取 Token（本机默认放开）。`} />
          </>
        )}
      </section>
    </>
  );
}

function AgentPanel({
  agents, sessions, onChanged, onError, onClose,
}: {
  agents: AgentInfo[];
  sessions: AgentSession[];
  onChanged: () => Promise<void>;
  onError: (e: unknown) => void;
  onClose: () => void;
}) {
  const [name, setName] = useState("");
  const [role, setRole] = useState("worker");
  const [caps, setCaps] = useState("");
  const [freshToken, setFreshToken] = useState<{ id: string; token: string } | null>(null);

  const run = async (fn: () => Promise<unknown>) => {
    try {
      const r = await fn() as { id?: string; token?: string } | null;
      if (r?.token) setFreshToken({ id: r.id!, token: r.token });
      await onChanged();
    } catch (e) { onError(e); }
  };

  return (
    <section className="panel">
      <div className="panel-head">
        <h3>Agent 管理</h3>
        <button className="close" onClick={onClose}>×</button>
      </div>
      <div className="tab-hint">在编 Agent 的生命周期管理：Token 轮换/吊销、会话出勤。接入新 Agent 请用侧栏的「⇄ 接入 Agent」。</div>
      {freshToken && (
        <div className="token-box">
          <b>新 Token（仅此一次显示，请立即保存）：</b>
          <code>{freshToken.token}</code>
          <button className="btn btn-success btn-sm" onClick={() => { navigator.clipboard?.writeText(freshToken.token); setFreshToken(null); }}>
            复制并关闭
          </button>
        </div>
      )}
      {agents.map((a) => {
        const agentSessions = sessions.filter((s) => s.agent_id === a.id).slice(0, 5);
        return (
          <div key={a.id} className="agent-block">
            <div className="panel-row">
              <span>{a.online ? "🟢" : "⚪"} <b>{a.name}</b></span>
              <span className="tag">{a.role}</span>
              <span className="muted">{(a.capabilities ?? []).join(", ") || "无能力标签"}</span>
              <span className="muted">{a.token_set ? "已签发 Token" : "未签发 Token"}</span>
              {a.holding_cards.length > 0 && <span className="muted">🔒 {a.holding_cards.length} 卡</span>}
              <div className="spacer" />
              {a.revoked ? <span className="error-text">已吊销</span> : (
                <>
                  <button className="btn btn-sm" onClick={() => run(() => api.rotateToken(a.id))}>轮换 Token</button>
                  <button className="btn btn-danger btn-sm" onClick={() => run(() => api.revokeAgent(a.id))}>吊销</button>
                </>
              )}
            </div>
            {/* Session 资源视图：编制下的每次出勤 */}
            {agentSessions.map((s) => (
              <div key={s.id} className="session-row">
                <span>{s.status === "active" ? "🟢" : s.status === "stale" ? "🟡" : "⚫"}</span>
                <span className="mono">{s.id.slice(0, 14)}</span>
                {s.branch && <span className="tag">{s.branch}</span>}
                {s.repo_path && <span className="muted mono">{s.repo_path.split("/").pop()}</span>}
                {s.holding_cards.length > 0 && <span className="muted">🔒 {s.holding_cards.length}</span>}
                <span className="muted">
                  {s.status === "ended" ? `结束于 ${s.ended_at}` : `心跳 ${s.last_heartbeat ?? "—"}`}
                </span>
                {s.parent_session_id && <span className="muted">⇠ {s.parent_session_id.slice(0, 14)}</span>}
              </div>
            ))}
            {agentSessions.length === 0 && <div className="session-row muted">无会话记录</div>}
          </div>
        );
      })}
      <div className="form-row">
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder="Agent 名称" />
        <select value={role} onChange={(e) => setRole(e.target.value)}>
          <option value="worker">worker</option>
          <option value="coordinator">coordinator</option>
          <option value="observer">observer</option>
        </select>
        <input value={caps} onChange={(e) => setCaps(e.target.value)} placeholder="能力标签，逗号分隔" />
        <button className="btn btn-primary" onClick={() => {
          if (name.trim()) run(() => api.createAgent(name.trim(), role,
            caps.split(",").map((c) => c.trim()).filter(Boolean)));
          setName(""); setCaps("");
        }}>＋ 注册并签发 Token</button>
      </div>
    </section>
  );
}

function DepsAdder({ card, board, run }: { card: CardDetail; board: BoardState | null; run: (fn: () => Promise<unknown>) => void }) {
  const [open, setOpen] = useState(false);
  const [rel, setRel] = useState("blocked_by");
  const [kw, setKw] = useState("");
  if (!open) {
    return (
      <div className="deps-add">
        <button className="btn-link" onClick={() => setOpen(true)}>＋ 添加依赖</button>
      </div>
    );
  }
  // 搜索选择器：人认标题不认 id；列出同看板其他卡片，关键字过滤（标题/id）
  const all = (board?.lists ?? []).flatMap((l) => l.cards).filter((c) => c.id !== card.id);
  const matches = all
    .filter((c) => !kw || c.title.toLowerCase().includes(kw.toLowerCase()) || c.id.includes(kw))
    .slice(0, 8);
  const pick = (otherId: string) => {
    run(() => api.addDep(card.id, otherId, rel));
    setOpen(false);
    setKw("");
  };
  return (
    <div className="deps-picker">
      <div className="form-row">
        <select value={rel} onChange={(e) => setRel(e.target.value)}>
          <option value="blocked_by">阻塞于我（我依赖它）</option>
          <option value="blocks">我阻塞它</option>
          <option value="relates_to">相关</option>
        </select>
        <input autoFocus value={kw} onChange={(e) => setKw(e.target.value)} placeholder="搜索卡片标题或 id" />
        <button className="btn btn-sm" onClick={() => { setOpen(false); setKw(""); }}>取消</button>
      </div>
      <div className="deps-candidates">
        {matches.map((c) => (
          <button key={c.id} className="deps-candidate" onClick={() => pick(c.id)}>
            <span>{c.title}</span>
            <span className="muted">{c.id}</span>
          </button>
        ))}
        {matches.length === 0 && <div className="muted">无匹配卡片</div>}
      </div>
    </div>
  );
}

function ArtifactsTab({ card, run }: { card: CardDetail; run: (fn: () => Promise<unknown>) => void }) {
  const [name, setName] = useState("");
  const [content, setContent] = useState("");
  const [view, setView] = useState<{ name: string; content: string } | null>(null);

  const open = async (id: string, name: string) => {
    try {
      const a = await api.artifact(id);
      setView(a.content != null ? { name: a.name, content: a.content } : { name: a.name, content: "（二进制或超大文件，无内联预览）" });
    } catch { setView({ name, content: "读取失败" }); }
  };

  return (
    <div>
      <div className="tab-hint">Agent 产出的文件（计划/报告/补丁），人在这里验收，可直接预览文本内容。</div>
      <h4>产物（{card.artifacts.length}）</h4>
      {card.artifacts.map((a) => (
        <div key={a.id} className="kv-row">
          <span className="tag">{a.kind}</span>
          <a href="#" onClick={(e) => { e.preventDefault(); open(a.id, a.name); }}>{a.name}</a>
          <span className="muted">{a.size_bytes} B · {memberName(a.uploaded_by)} · {a.uploaded_at}</span>
        </div>
      ))}
      {card.artifacts.length === 0 && <div className="muted">暂无产物</div>}
      {view && (
        <div className="handoff-pkg">
          <h4>{view.name}</h4>
          <pre className="artifact-view">{view.content}</pre>
          <button className="btn btn-sm" onClick={() => setView(null)}>收起</button>
        </div>
      )}
      <div className="form-row">
        <input value={name} onChange={(e) => setName(e.target.value)} placeholder="产物名，如 plan.md" />
      </div>
      <div className="form-row">
        <textarea
          value={content}
          onChange={(e) => setContent(e.target.value)}
          placeholder="文本内容（Agent 可通过 MCP artifact_upload 传 path/content）"
          rows={4}
          style={{ flex: 1, minWidth: 240 }}
        />
        <button className="btn btn-sm" onClick={() => { if (name.trim()) run(() => api.uploadArtifact(card.id, { name: name.trim(), content })); }}>
          📎 上传
        </button>
      </div>
    </div>
  );
}

function HandoffTab({card, state, run,
}: {
  card: CardDetail;
  state: string;
  run: (fn: () => Promise<unknown>) => void;
}) {
  const [note, setNote] = useState("");
  const h = card.handoff;
  return (
    <div>
      <div className="tab-hint">把这张卡的上下文（进展/卡点/未结话题）打包交接给另一个 Agent 或未来的自己。</div>
      <div className="handoff-state">当前状态：<b>{state}</b></div>
      {state === "none" && (
        <div className="form-row">
          <input value={note} onChange={(e) => setNote(e.target.value)}
            placeholder="移交说明：进展/卡点/下一步（Markdown）" />
          <button className="btn btn-sm" onClick={() => run(() => api.handoff(card.id, "prepare", { context_note: note, reason: note }))}>
            📦 准备移交
          </button>
        </div>
      )}
      {state === "preparing" && (
        <div className="actions">
          <button className="btn btn-success btn-sm" onClick={() => run(() => api.handoff(card.id, "ready"))}>✅ 移交就绪（释放租约）</button>
          <button className="btn btn-sm" onClick={() => run(() => api.handoff(card.id, "cancel"))}>取消</button>
        </div>
      )}
      {state === "ready" && (
        <div className="actions">
          <button className="btn btn-success btn-sm" onClick={() => run(() => api.handoff(card.id, "accept"))}>🤝 接受移交（code-agent）</button>
          <button className="btn btn-sm" onClick={() => run(() => api.handoff(card.id, "cancel"))}>取消</button>
        </div>
      )}
      {h?.package && (
        <div className="handoff-pkg">
          <h4>移交包</h4>
          <p>{h.package.context_note}</p>
          {h.package.env_notes && <p className="muted">环境：{h.package.env_notes}</p>}
          <div className="muted">未结话题 {h.package.open_threads?.length ?? 0} 个</div>
        </div>
      )}
      {h && h.timeline.length > 0 && (
        <>
          <h4>移交时间线</h4>
          {h.timeline.map((t, i) => (
            <div key={i} className="kv-row">
              <span className="tag">{t.action}</span>
              <span>{memberName(t.by_id)}</span>
              <span className="muted">{t.at}</span>
            </div>
          ))}
        </>
      )}
    </div>
  );
}
