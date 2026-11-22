import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type FormEvent, type ReactNode } from 'react'
import {
  addNote,
  getDraft,
  getTicket,
  listAgents,
  listCustomers,
  listCustomerServices,
  moveTicket,
  reply,
  setAssignee,
  setPriority,
  setState,
  Unauthorized,
  type AgentEntry,
  type CustomerEntry,
  type Moved,
  type Priority,
  type Service,
  type TicketDetail,
  type TicketState,
  type TimelineEntry,
} from './api/client'
import { useI18n } from './i18n/context'
import { addonName } from './addon'
import { useDraftKeeper, type SaveState } from './drafts'
import { TicketProposals } from './Proposals'
import { useSession } from './session'
import { TicketAssets, TicketFamily, TicketServices } from './TicketLinks'
import { useLoad } from './useAction'

const states: TicketState[] = ['open', 'waiting', 'resolved', 'closed']
const priorities: Priority[] = ['low', 'normal', 'high', 'urgent']

interface Props {
  number: number
  /** Where "back" goes: the ticket list as it was last filtered. */
  backHref: string
  /** Called after a change, so the lists follow it. */
  onChanged: () => void
  onSignedOut: () => void
}

export function TicketView({ number, backHref, onChanged, onSignedOut }: Props) {
  const { t, formatTime, errorText } = useI18n()
  const { agent, can } = useSession()
  const canWork = can('tickets.work')
  const allowed: Allowed = { reply: can('tickets.reply'), note: canWork }
  const [ticket, setTicket] = useState<TicketDetail | null>(null)
  // Failures are kept as they came and put into words when shown, so they follow the language.
  const [loadError, setLoadError] = useState<unknown>(null)
  const [agents, setAgents] = useState<AgentEntry[]>([])
  // Customers that can get tickets, for moving the ticket to one of them.
  const [customers, setCustomers] = useState<CustomerEntry[] | null>(null)
  const [actionError, setActionError] = useState<unknown>(null)
  const [busy, setBusy] = useState(false)
  const [moving, setMoving] = useState(false)
  // What the last move did: how many tickets it took along, how many links it removed.
  const [moved, setMoved] = useState<Moved | null>(null)
  // Only the latest load may update the view: a slow poll must not overwrite a newer answer.
  const latest = useRef(0)
  // The composer's text and mode live here, so that an accepted draft reply can be put into it.
  const [draft, setDraft] = useState('')
  // null until the agent picks: reply when there is a requester, else note.
  const [chosen, setChosen] = useState<Mode | null>(null)
  // The add-on whose accepted draft is in the composer, unsent.
  const [draftFrom, setDraftFrom] = useState<string | null>(null)
  // Asks the composer to put the caret after a text put into it (a restored or accepted draft), and,
  // for an accepted one, to come into view with the focus.
  const [caretAsked, setCaretAsked] = useState<CaretRequest | null>(null)
  // The composer's text is kept on the core as the agent's draft.
  const keeper = useDraftKeeper(number, onSignedOut)
  const { known } = keeper
  // Set once the agent types or picks a mode: a draft loaded after that does not overwrite it.
  const touched = useRef(false)

  const fail = useCallback((error: unknown, show: (error: unknown) => void) => {
    if (error instanceof Unauthorized) onSignedOut()
    else show(error)
  }, [onSignedOut])

  const load = useCallback(async () => {
    const mine = ++latest.current
    try {
      const detail = await getTicket(number)
      if (mine !== latest.current) return
      setTicket(detail)
      setLoadError(null)
    } catch (error) {
      if (mine === latest.current) fail(error, setLoadError)
    }
  }, [number, fail])

  useEffect(() => {
    // Polled like the lists, so that a queued reply shows when it leaves.
    const first = setTimeout(load, 0)
    const timer = setInterval(load, 10_000)
    return () => {
      clearTimeout(first)
      clearInterval(timer)
    }
  }, [load])

  useEffect(() => {
    // The agent's unsent text, from when they last left this ticket.
    let gone = false
    getDraft(number).then((saved) => {
      if (gone || touched.current) return
      if (saved) {
        setDraft(saved.body)
        setChosen(saved.mode)
        setCaretAsked((r) => ({ seq: (r?.seq ?? 0) + 1, focus: false }))
        known({ mode: saved.mode, body: saved.body })
      } else {
        known({ mode: 'reply', body: '' })
      }
    }, (error) => fail(error, setActionError))
    return () => { gone = true }
  }, [number, fail, known])

  useEffect(() => {
    listAgents().then(setAgents, (error) => fail(error, setActionError))
    listCustomers().then(setCustomers, (error) => fail(error, setActionError))
  }, [fail])

  /** After a change made by one of the parts below: reloads the ticket and the lists. */
  const changed = useCallback(async () => {
    await load()
    onChanged()
  }, [load, onChanged])

  /** Runs a change, then reloads the ticket and the lists. Returns whether it succeeded. */
  async function change(action: () => Promise<void>): Promise<boolean> {
    setBusy(true)
    setActionError(null)
    try {
      await action()
      return true
    } catch (error) {
      fail(error, setActionError)
      return false
    } finally {
      setBusy(false)
      await load()
      onChanged()
    }
  }

  /** Puts an accepted draft reply into the composer, in reply mode; it is not sent. */
  function takeDraft(addon: string, body: string) {
    setDraft(body)
    setChosen('reply')
    setDraftFrom(addon)
    // Accepting saved it as this agent's draft; saving it again, after any save still on its way,
    // makes sure an older text typed before cannot land last.
    keeper.changed({ mode: 'reply', body })
    keeper.flush()
    setCaretAsked((r) => ({ seq: (r?.seq ?? 0) + 1, focus: true }))
  }

  function changeState(state: TicketState) {
    if (state === 'closed' && !window.confirm(t('ticket.confirmClose', { number }))) return
    void change(() => setState(number, state))
  }

  if (!ticket) {
    return (
      <div className="ticket">
        <a className="back" href={backHref}>{t('ticket.back')}</a>
        {loadError !== null && <p className="error" role="alert">{errorText(loadError)}</p>}
      </div>
    )
  }

  const closed = ticket.state === 'closed'
  const requester = ticket.requester
  const assigneeId = ticket.assignee?.id ?? null
  /**
   * Whether the agent may make `target` the assignee (null: nobody). Taking the ticket, or giving
   * back one's own, is working on it; anything else is assigning it to others.
   */
  const mayAssign = (target: string | null) => {
    const own = target === null ? assigneeId === null || assigneeId === agent.id : target === agent.id
    return can(own ? 'tickets.work' : 'tickets.assign')
  }
  // The agents the role lets the agent pick, and the current assignee whoever it is.
  const assignable = agents.filter((a) => a.id === assigneeId || mayAssign(a.id))
  const canChangeAssignee = mayAssign(null) || agents.some((a) => a.id !== assigneeId && mayAssign(a.id))
  const writable = allowed.note || replyPossible(ticket, allowed)
  return (
    <div className="ticket">
      <a className="back" href={backHref}>{t('ticket.back')}</a>
      <h1><span className="number">#{ticket.number}</span> {ticket.title}</h1>
      {loadError !== null && <p className="error" role="alert">{errorText(loadError)}</p>}

      <dl className="facts">
        <div>
          <dt>{t('ticket.customer')}</dt>
          <dd>
            {ticket.customer.name}{' '}
            {!closed && !moving && can('tickets.move') && (
              <button type="button" className="link" disabled={busy} onClick={() => setMoving(true)}>
                {t('ticket.changeCustomer')}
              </button>
            )}
          </dd>
        </div>
        <div>
          <dt>{t('ticket.requester')}</dt>
          <dd>
            {requester
              ? requester.name ? <>{requester.name} <span className="muted">&lt;{requester.email}&gt;</span></> : requester.email
              : <span className="muted">{t('ticket.noRequester')}</span>}
          </dd>
        </div>
        <div>
          <dt><label htmlFor="ticket-state">{t('ticket.state')}</label></dt>
          <dd>
            <select id="ticket-state" className={`state ${ticket.state}`} value={ticket.state}
              disabled={closed || busy || !canWork} onChange={(e) => changeState(e.target.value as TicketState)}>
              {states.map((s) => <option key={s} value={s}>{t(`state.${s}`)}</option>)}
            </select>
          </dd>
        </div>
        <div>
          <dt><label htmlFor="ticket-priority">{t('ticket.priority')}</label></dt>
          <dd>
            <select id="ticket-priority" className={`priority ${ticket.priority}`} value={ticket.priority}
              disabled={closed || busy || !canWork}
              onChange={(e) => void change(() => setPriority(number, e.target.value as Priority))}>
              {priorities.map((p) => <option key={p} value={p}>{t(`priority.${p}`)}</option>)}
            </select>
          </dd>
        </div>
        <div>
          <dt><label htmlFor="ticket-assignee">{t('ticket.assignee')}</label></dt>
          <dd>
            <select id="ticket-assignee" value={assigneeId ?? ''} disabled={closed || busy || !canChangeAssignee}
              onChange={(e) => void change(() => setAssignee(number, e.target.value || null))}>
              {(assigneeId === null || mayAssign(null)) && <option value="">{t('ticket.unassigned')}</option>}
              {/* The current assignee stays visible even before the agents list arrives. */}
              {ticket.assignee && !agents.some((a) => a.id === assigneeId) && (
                <option value={ticket.assignee.id}>{ticket.assignee.name}</option>
              )}
              {assignable.map((a) => <option key={a.id} value={a.id}>{a.name}</option>)}
            </select>
          </dd>
        </div>
        <div><dt>{t('ticket.opened')}</dt><dd className="when">{formatTime(ticket.created_at)}</dd></div>
      </dl>
      {moving && (
        <MoveCustomer ticket={ticket} customers={customers} busy={busy} onCancel={() => setMoving(false)}
          onSignedOut={onSignedOut}
          onMove={async (customerId, requesterMove) => {
            let result: Moved | null = null
            const ok = await change(async () => {
              result = await moveTicket(number, customerId, requesterMove)
            })
            if (ok) {
              setMoving(false)
              setMoved(result)
            }
          }} />
      )}
      {moved !== null && (
        <p className="done" role="status">
          {moved.moved > 1 ? t('ticket.movedWithRequester', { count: moved.moved }) : t('ticket.moved')}
          {moved.unlinked > 0 && <> {t('ticket.unlinked', { count: moved.unlinked })}</>}
        </p>
      )}
      {actionError !== null && <p className="error" role="alert">{errorText(actionError)}</p>}

      <TicketProposals ticket={ticket} draftInUse={draft.trim() !== ''} onReplyDraft={takeDraft}
        onChanged={changed} onSignedOut={onSignedOut} />

      <div className="ticket-links">
        <TicketAssets ticket={ticket} onChanged={changed} onSignedOut={onSignedOut} />
        <TicketServices ticket={ticket} onChanged={changed} onSignedOut={onSignedOut} />
        <TicketFamily ticket={ticket} onChanged={changed} onSignedOut={onSignedOut} />
      </div>

      <ol className="timeline">
        {ticket.timeline.map((entry) => <Entry key={`${entry.kind}-${entry.id}`} entry={entry} />)}
        {ticket.timeline.length === 0 && <li className="empty">{t('ticket.emptyTimeline')}</li>}
      </ol>

      {closed && (
        <p className="closed-notice">{t('ticket.closedNotice')}</p>
      )}
      {!writable && <p className="notice">{t('composer.notAllowed')}</p>}
      {writable && (
        <Composer ticket={ticket} allowed={allowed} body={draft} chosen={chosen} draftFrom={draftFrom}
          caretAsked={caretAsked} saveState={keeper.saveState}
          onBody={(body) => {
            touched.current = true
            setDraft(body)
            if (!body.trim()) setDraftFrom(null)
            keeper.changed({ mode: composerMode(ticket, chosen, allowed), body })
          }}
          onChoose={(mode) => {
            touched.current = true
            setChosen(mode)
            keeper.changed({ mode: composerMode(ticket, mode, allowed), body: draft })
          }}
          onLeave={keeper.flush}
          onDone={(mode) => {
            // Sending cleared the draft on the core.
            known({ mode, body: '' })
            setDraftFrom(null)
            void load()
            onChanged()
          }}
          onSignedOut={onSignedOut} />
      )}
    </div>
  )
}

function Entry({ entry }: { entry: TimelineEntry }) {
  const { t, formatTime } = useI18n()
  if (entry.kind === 'note') {
    return (
      <li className="entry note">
        <div className="entry-head">
          <span className="tag note-tag">{t('entry.note')}</span>
          <span className="who">{entry.author_name}</span>
          <time className="when" dateTime={entry.created_at}>{formatTime(entry.created_at)}</time>
        </div>
        <p className="body">{entry.body}</p>
      </li>
    )
  }
  const outbound = entry.direction === 'outbound'
  const sender = entry.from_name ? `${entry.from_name} <${entry.from_address}>` : entry.from_address
  return (
    <li className={`entry message ${outbound ? 'outbound' : 'inbound'}`}>
      <div className="entry-head">
        <span className="tag">{outbound ? t('entry.outgoing') : t('entry.incoming')}</span>
        <span className="who">
          {outbound ? <>{entry.author_name ?? sender} <span className="muted">{t('entry.to', { address: entry.to_address ?? '—' })}</span></> : sender}
        </span>
        {outbound && (entry.sent_at
          ? <span className="status sent">{t('entry.sent', { time: formatTime(entry.sent_at) })}</span>
          : entry.gave_up
            ? <span className="status failed">{t('entry.notSent')}</span>
            : <span className="status queued">{t('entry.queued')}</span>)}
        <time className="when" dateTime={entry.created_at}>{formatTime(entry.created_at)}</time>
      </div>
      {entry.subject && <div className="subject">{entry.subject}</div>}
      <p className="body">{entry.body_text}</p>
      {outbound && !entry.sent_at && entry.gave_up && (
        <p className="error" role="alert">
          {t('entry.gaveUp', { error: entry.last_error ?? t('entry.unknownError') })}
        </p>
      )}
      {outbound && !entry.sent_at && !entry.gave_up && entry.last_error && (
        <p className="error">{t('entry.retrying', { error: entry.last_error })}</p>
      )}
    </li>
  )
}

type Mode = 'reply' | 'note'
const strong = (inner: ReactNode) => <strong>{inner}</strong>
// Closed is left out on purpose: closing is final and goes through the state control and its
// confirmation.
const thenStates: TicketState[] = ['open', 'waiting', 'resolved']

/** Asks the composer to put the caret at the end of its text; `seq` tells one request from the next. */
interface CaretRequest {
  seq: number
  focus: boolean
}

/** What the agent's role lets them write: replies to requesters, internal notes. */
interface Allowed {
  reply: boolean
  note: boolean
}

/** Whether a reply can be written: the ticket has a requester, is not closed, and the role allows it. */
const replyPossible = (ticket: TicketDetail, allowed: Allowed) =>
  allowed.reply && Boolean(ticket.requester) && ticket.state !== 'closed'

/** What the composer writes: the agent's pick when both are possible, else the one that is. */
function composerMode(ticket: TicketDetail, picked: Mode | null, allowed: Allowed): Mode {
  if (!replyPossible(ticket, allowed)) return 'note'
  if (!allowed.note) return 'reply'
  return picked ?? 'reply'
}

/** The tallest the composer grows before it scrolls, in px. */
const composerMaxHeight = () => Math.min(window.innerHeight * 0.6, 520)

function Composer({ ticket, allowed, body, chosen: picked, draftFrom, caretAsked, saveState, onBody, onChoose, onLeave, onDone, onSignedOut }: {
  ticket: TicketDetail
  /** What the agent's role lets them write; the composer is shown only when one of them is possible. */
  allowed: Allowed
  body: string
  /** The mode the agent picked; null for the default. The poll never changes it. */
  chosen: Mode | null
  /** The add-on whose accepted draft reply is in the composer, not sent yet. */
  draftFrom: string | null
  /** A new request puts the caret after the text, and maybe brings the composer into view with the focus. */
  caretAsked: CaretRequest | null
  /** Whether the text is saved as the agent's draft. */
  saveState: SaveState
  onBody: (body: string) => void
  onChoose: (mode: Mode) => void
  /** The composer lost the focus: save the draft now. */
  onLeave: () => void
  /** Sent; `mode` is what was sent. */
  onDone: (mode: Mode) => void
  onSignedOut: () => void
}) {
  const { t, rich, errorText } = useI18n()
  const requester = ticket.requester
  const setChosen = onChoose
  const setBody = onBody
  const [then, setThen] = useState<TicketState>('waiting')
  // The address the agent picked, on a ticket the requester has not written on.
  const [pickedTo, setPickedTo] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<unknown>(null)
  const id = useId()
  // Without a requester there is nobody to reply to, and a closed ticket gets no more replies.
  const closed = ticket.state === 'closed'
  const canReply = replyPossible(ticket, allowed)
  const mode: Mode = composerMode(ticket, picked, allowed)
  const to = replyTo(requester, pickedTo)
  // The requester has not written on the ticket and has several addresses: the agent picks one.
  const pickAddress = mode === 'reply' && !!requester && !requester.reply_to && requester.addresses.length > 1

  const textarea = useRef<HTMLTextAreaElement>(null)

  // The text area grows with what is in it, so that a long draft shows whole, up to a limit.
  useLayoutEffect(() => {
    const area = textarea.current
    if (!area) return
    area.style.height = 'auto'
    const borders = area.offsetHeight - area.clientHeight
    area.style.height = `${Math.min(area.scrollHeight + borders, composerMaxHeight())}px`
  }, [body, mode])

  useEffect(() => {
    const area = textarea.current
    if (!caretAsked || !area) return
    if (caretAsked.focus) {
      area.scrollIntoView({ block: 'center' })
      area.focus()
    }
    // Typing goes on after the draft; the text stays scrolled to its start, greeting in view.
    area.setSelectionRange(area.value.length, area.value.length)
    area.scrollTop = 0
  }, [caretAsked])

  async function submit(event: FormEvent) {
    event.preventDefault()
    if (busy || !body.trim() || (mode === 'reply' && !to)) return
    setBusy(true)
    setError(null)
    try {
      if (mode === 'reply') await reply(ticket.number, body, then, requester?.reply_to ? null : to)
      else await addNote(ticket.number, body)
      setBody('')
      setThen('waiting')
      onDone(mode)
    } catch (e) {
      if (e instanceof Unauthorized) onSignedOut()
      else setError(e)
    } finally {
      setBusy(false)
    }
  }

  return (
    <form className={`composer ${mode}`} onSubmit={submit}>
      <fieldset className="modes">
        <legend className="visually-hidden">{t('composer.modes')}</legend>
        <label className={mode === 'reply' ? 'current' : ''}>
          <input type="radio" name={`${id}-mode`} value="reply" checked={mode === 'reply'}
            disabled={!canReply} onChange={() => setChosen('reply')} />
          {t('composer.reply')}
        </label>
        <label className={mode === 'note' ? 'current' : ''}>
          <input type="radio" name={`${id}-mode`} value="note" checked={mode === 'note'}
            disabled={!allowed.note} onChange={() => setChosen('note')} />
          {t('composer.note')}
        </label>
      </fieldset>
      <p className="mode-hint" id={`${id}-hint`}>
        {mode === 'reply' && requester
          ? requester.reply_to
            ? rich('composer.hintReplyWroteFrom', { email: requester.reply_to, strong })
            : to
              ? rich('composer.hintReply', { email: to, strong })
              : t('composer.hintPickAddress')
          : closed
            ? t('composer.hintClosed')
            : requester
              ? rich('composer.hintNote', { strong })
              : t('composer.hintNoRequester')}
      </p>
      <label className="visually-hidden" htmlFor={`${id}-body`}>
        {mode === 'reply' ? t('composer.replyLabel') : t('composer.noteLabel')}
      </label>
      {pickAddress && requester && (
        <label className="reply-to">
          {t('composer.to')}{' '}
          <select value={to ?? ''} required onChange={(e) => setPickedTo(e.target.value)}>
            {!to && <option value="" disabled>{t('composer.chooseAddress')}</option>}
            {requester.addresses.map((a) => <option key={a} value={a}>{a}</option>)}
          </select>
        </label>
      )}
      {draftFrom && mode === 'reply' && (
        <p className="draft-notice" role="status">
          {rich('proposals.draftReady', { addon: addonName(draftFrom), strong })}
        </p>
      )}
      <textarea ref={textarea} id={`${id}-body`} rows={5} value={body} aria-describedby={`${id}-hint`}
        placeholder={mode === 'reply' ? t('composer.replyPlaceholder') : t('composer.notePlaceholder')}
        onChange={(e) => setBody(e.target.value)} onBlur={onLeave} />
      {error !== null && <p className="error" role="alert">{errorText(error)}</p>}
      <div className="composer-foot">
        {mode === 'reply' && (
          <label>
            {t('composer.then')}{' '}
            <select value={then} onChange={(e) => setThen(e.target.value as TicketState)}>
              {thenStates.map((s) => <option key={s} value={s}>{t(`state.${s}`)}</option>)}
            </select>
          </label>
        )}
        {saveState?.kind === 'saving' && <span className="draft-status">{t('composer.draftSaving')}</span>}
        {saveState?.kind === 'saved' && <span className="draft-status">{t('composer.draftSaved')}</span>}
        {saveState?.kind === 'failed' && (
          <span className="draft-status error" role="alert">
            {t('composer.draftNotSaved', { error: errorText(saveState.error) })}
          </span>
        )}
        <button type="submit" disabled={busy || !body.trim() || (mode === 'reply' && !to)}>
          {mode !== 'reply' ? t('composer.addNote') : to ? t('composer.sendReply', { email: to }) : t('composer.send')}
        </button>
      </div>
    </form>
  )
}

/**
 * Where a reply goes: the address the requester last wrote from on the ticket; else the one the
 * agent picks here, or picked for the last reply, while it is still theirs; else their only one.
 * Null while an agent still has to pick.
 */
function replyTo(requester: TicketDetail['requester'], picked: string | null): string | null {
  if (!requester) return null
  if (requester.reply_to) return requester.reply_to
  if (picked && requester.addresses.includes(picked)) return picked
  if (requester.last_picked) return requester.last_picked
  return requester.addresses.length === 1 ? requester.addresses[0] : null
}

/** Moves the ticket to the right customer, and says what happens to the requester. */
function MoveCustomer({ ticket, customers, busy, onCancel, onMove, onSignedOut }: {
  ticket: TicketDetail
  customers: CustomerEntry[] | null
  busy: boolean
  onCancel: () => void
  onMove: (customerId: string, requester: 'move' | 'remove') => Promise<void>
  onSignedOut: () => void
}) {
  const { t, errorText } = useI18n()
  const [customerId, setCustomerId] = useState('')
  const [requesterMove, setRequesterMove] = useState<'move' | 'remove'>('move')
  const id = useId()
  const requester = ticket.requester
  const others = requester ? requester.ticket_count - 1 : 0
  const linked = ticket.assets.length + ticket.services.length > 0
  // What the chosen customer uses decides which linked services stay.
  const loadUsed = useCallback(
    (): Promise<Service[]> => (linked && customerId ? listCustomerServices(customerId) : Promise.resolve([])),
    [linked, customerId])
  const used = useLoad(loadUsed, onSignedOut)
  const target = customers?.find((c) => c.id === customerId)
  const dropped = target && used.data
    ? ticket.services.filter((s) => !used.data?.some((u) => u.id === s.id))
    : null
  const error = used.error

  function submit(event: FormEvent) {
    event.preventDefault()
    if (customerId) void onMove(customerId, requesterMove)
  }

  return (
    <form className="move" onSubmit={submit}>
      <label htmlFor={`${id}-customer`}>{t('move.to')}</label>
      <select id={`${id}-customer`} value={customerId} onChange={(e) => setCustomerId(e.target.value)}>
        <option value="">{t('customer.choose')}</option>
        {customers?.filter((c) => c.id !== ticket.customer.id).map((c) => (
          <option key={c.id} value={c.id}>{c.is_help_desk ? t('customer.helpDesk', { name: c.name }) : c.name}</option>
        ))}
      </select>
      {requester && (
        <fieldset>
          <legend>{t('move.requesterBelongs', { email: requester.email, customer: ticket.customer.name })}</legend>
          <label>
            <input type="radio" name={`${id}-requester`} checked={requesterMove === 'move'}
              onChange={() => setRequesterMove('move')} />
            {others > 0
              ? t('move.requesterTooWithTickets', { count: others })
              : t('move.requesterToo')}
          </label>
          <label>
            <input type="radio" name={`${id}-requester`} checked={requesterMove === 'remove'}
              onChange={() => setRequesterMove('remove')} />
            {t('move.requesterStays', { customer: ticket.customer.name })}
          </label>
        </fieldset>
      )}
      {linked && (
        <div className="warning" role="note">
          {!target || !dropped ? (
            <p>{t('move.linksWarning', { customer: ticket.customer.name })}</p>
          ) : ticket.assets.length + dropped.length === 0 ? (
            <p>{t('move.keepsLinks', { customer: target.name })}</p>
          ) : (
            <>
              <p>{t('move.willUnlink')}</p>
              <ul>
                {ticket.assets.map((a) => (
                  <li key={a.id}>{t('move.unlinkAsset', { name: a.name, kind: a.kind, customer: ticket.customer.name })}</li>
                ))}
                {dropped.map((s) => (
                  <li key={s.id}>{t('move.unlinkService', { name: s.name, customer: target.name })}</li>
                ))}
              </ul>
            </>
          )}
        </div>
      )}
      {error !== null && <p className="error" role="alert">{errorText(error)}</p>}
      <div className="move-foot">
        <button type="button" onClick={onCancel}>{t('move.cancel')}</button>
        <button type="submit" disabled={busy || !customerId}>{t('move.submit')}</button>
      </div>
    </form>
  )
}
