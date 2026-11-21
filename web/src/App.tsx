import { useCallback, useEffect, useState, type FormEvent } from 'react'
import {
  currentAgent,
  listInbox,
  listTickets,
  signIn,
  signOut,
  Unauthorized,
  type Agent,
  type InboxMessage,
  type TicketSummary,
} from './api/client'
import { useI18n } from './i18n/context'
import { LanguageSwitcher } from './i18n/LanguageSwitcher'
import { Inbox } from './Inbox'
import { go, inboxHref, ticketHref, ticketsHref, useRoute } from './route'
import { TicketView } from './TicketView'

function App() {
  // undefined: still asking the core; null: nobody signed in.
  const { t } = useI18n()
  const [agent, setAgent] = useState<Agent | null | undefined>(undefined)
  const [unreachable, setUnreachable] = useState(false)

  useEffect(() => {
    currentAgent().then(setAgent, () => setUnreachable(true))
  }, [])

  const signedOut = useCallback(() => setAgent(null), [])

  if (unreachable) return <main className="center"><p className="error">{t('error.unreachable')}</p></main>
  if (agent === undefined) return null
  if (agent === null) return <SignIn onSignedIn={setAgent} />
  return <Desk agent={agent} onSignedOut={signedOut} />
}

function SignIn({ onSignedIn }: { onSignedIn: (agent: Agent) => void }) {
  const { t } = useI18n()
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [failed, setFailed] = useState<'wrong' | 'unreachable' | null>(null)
  const [busy, setBusy] = useState(false)

  async function submit(event: FormEvent) {
    event.preventDefault()
    setBusy(true)
    setFailed(null)
    try {
      if (await signIn(email, password)) {
        const agent = await currentAgent()
        if (agent) onSignedIn(agent)
      } else {
        setFailed('wrong')
      }
    } catch {
      setFailed('unreachable')
    } finally {
      setBusy(false)
    }
  }

  return (
    <main className="center">
      <form className="sign-in" onSubmit={submit}>
        <h1>{t('app.title')}</h1>
        <label>
          {t('signIn.email')}
          <input type="email" autoComplete="username" required value={email}
            onChange={(e) => setEmail(e.target.value)} />
        </label>
        <label>
          {t('signIn.password')}
          <input type="password" autoComplete="current-password" required value={password}
            onChange={(e) => setPassword(e.target.value)} />
        </label>
        {failed && (
          <p className="error" role="alert">
            {failed === 'wrong' ? t('signIn.wrong') : t('error.unreachable')}
          </p>
        )}
        <button type="submit" disabled={busy}>{t('signIn.submit')}</button>
        <LanguageSwitcher />
      </form>
    </main>
  )
}

function Desk({ agent, onSignedOut }: { agent: Agent; onSignedOut: () => void }) {
  const { t, formatNumber } = useI18n()
  const route = useRoute()
  const [tickets, setTickets] = useState<TicketSummary[] | null>(null)
  const [inbox, setInbox] = useState<InboxMessage[] | null>(null)
  const [loadFailed, setLoadFailed] = useState(false)

  const refresh = useCallback(async () => {
    try {
      const [t, i] = await Promise.all([listTickets(), listInbox()])
      setTickets(t)
      setInbox(i)
      setLoadFailed(false)
    } catch (e) {
      if (e instanceof Unauthorized) onSignedOut()
      else setLoadFailed(true)
    }
  }, [onSignedOut])

  useEffect(() => {
    // Mail is read every few seconds, so the lists follow it.
    const first = setTimeout(refresh, 0)
    const timer = setInterval(refresh, 10_000)
    return () => {
      clearTimeout(first)
      clearInterval(timer)
    }
  }, [refresh])

  async function leave() {
    await signOut().catch(() => {})
    onSignedOut()
  }

  return (
    <div className="desk">
      <header>
        <nav>
          <button className={route.view === 'inbox' ? '' : 'current'} onClick={() => go(ticketsHref)}>
            {t('desk.tickets')} {tickets && <span className="count">{formatNumber(tickets.length)}</span>}
          </button>
          <button className={route.view === 'inbox' ? 'current' : ''} onClick={() => go(inboxHref)}>
            {t('desk.inbox')} {inbox && <span className="count">{formatNumber(inbox.length)}</span>}
          </button>
        </nav>
        <span className="who">{agent.name}</span>
        <LanguageSwitcher />
        <button className="quiet" onClick={leave}>{t('desk.signOut')}</button>
      </header>
      <main>
        {loadFailed && <p className="error" role="alert">{t('error.loadFailed')}</p>}
        {route.view === 'ticket' ? (
          <TicketView key={route.number} number={route.number} onChanged={refresh}
            onSignedOut={onSignedOut} />
        ) : route.view === 'inbox' ? (
          <Inbox messages={inbox} onChanged={refresh} onSignedOut={onSignedOut} />
        ) : (
          <Tickets tickets={tickets} />
        )}
      </main>
    </div>
  )
}

function Tickets({ tickets }: { tickets: TicketSummary[] | null }) {
  const { t, formatTime } = useI18n()
  if (tickets === null) return null
  if (tickets.length === 0) return <p className="empty">{t('tickets.empty')}</p>
  return (
    <table className="tickets">
      <thead>
        <tr>
          <th>{t('tickets.number')}</th><th>{t('tickets.title')}</th><th>{t('tickets.customer')}</th>
          <th>{t('tickets.requester')}</th><th>{t('tickets.assignee')}</th><th>{t('tickets.state')}</th>
          <th>{t('tickets.updated')}</th>
        </tr>
      </thead>
      <tbody>
        {tickets.map((ticket) => (
          // The title is the link; the rest of the row is a larger target for the mouse.
          <tr key={ticket.id} onClick={() => go(ticketHref(ticket.number))}>
            <td className="number">{ticket.number}</td>
            <td><a href={ticketHref(ticket.number)}>{ticket.title}</a></td>
            <td>{ticket.customer_name}</td>
            <td>{ticket.requester_email ?? '—'}</td>
            <td>{ticket.assignee_name ?? '—'}</td>
            <td><span className={`state ${ticket.state}`}>{t(`state.${ticket.state}`)}</span></td>
            <td className="when">{formatTime(ticket.updated_at)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

export default App
