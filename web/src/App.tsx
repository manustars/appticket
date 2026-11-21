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
import { formatTime, stateLabels } from './format'
import { Inbox } from './Inbox'
import { go, inboxHref, ticketHref, ticketsHref, useRoute } from './route'
import { TicketView } from './TicketView'

function App() {
  // undefined: still asking the core; null: nobody signed in.
  const [agent, setAgent] = useState<Agent | null | undefined>(undefined)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    currentAgent().then(setAgent, () => setError('The core is not reachable.'))
  }, [])

  const signedOut = useCallback(() => setAgent(null), [])

  if (error) return <main className="center"><p className="error">{error}</p></main>
  if (agent === undefined) return null
  if (agent === null) return <SignIn onSignedIn={setAgent} />
  return <Desk agent={agent} onSignedOut={signedOut} />
}

function SignIn({ onSignedIn }: { onSignedIn: (agent: Agent) => void }) {
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [failed, setFailed] = useState<string | null>(null)
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
        setFailed('Wrong e-mail or password.')
      }
    } catch {
      setFailed('The core is not reachable.')
    } finally {
      setBusy(false)
    }
  }

  return (
    <main className="center">
      <form className="sign-in" onSubmit={submit}>
        <h1>Help desk</h1>
        <label>
          E-mail
          <input type="email" autoComplete="username" required value={email}
            onChange={(e) => setEmail(e.target.value)} />
        </label>
        <label>
          Password
          <input type="password" autoComplete="current-password" required value={password}
            onChange={(e) => setPassword(e.target.value)} />
        </label>
        {failed && <p className="error" role="alert">{failed}</p>}
        <button type="submit" disabled={busy}>Sign in</button>
      </form>
    </main>
  )
}

function Desk({ agent, onSignedOut }: { agent: Agent; onSignedOut: () => void }) {
  const route = useRoute()
  const [tickets, setTickets] = useState<TicketSummary[] | null>(null)
  const [inbox, setInbox] = useState<InboxMessage[] | null>(null)
  const [error, setError] = useState<string | null>(null)

  const refresh = useCallback(async () => {
    try {
      const [t, i] = await Promise.all([listTickets(), listInbox()])
      setTickets(t)
      setInbox(i)
      setError(null)
    } catch (e) {
      if (e instanceof Unauthorized) onSignedOut()
      else setError('Could not load from the core.')
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
            Tickets {tickets && <span className="count">{tickets.length}</span>}
          </button>
          <button className={route.view === 'inbox' ? 'current' : ''} onClick={() => go(inboxHref)}>
            Inbox {inbox && <span className="count">{inbox.length}</span>}
          </button>
        </nav>
        <span className="who">{agent.name}</span>
        <button className="quiet" onClick={leave}>Sign out</button>
      </header>
      <main>
        {error && <p className="error" role="alert">{error}</p>}
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
  if (tickets === null) return null
  if (tickets.length === 0) return <p className="empty">No tickets yet.</p>
  return (
    <table className="tickets">
      <thead>
        <tr>
          <th>#</th><th>Title</th><th>Customer</th><th>Requester</th><th>Assignee</th><th>State</th>
          <th>Updated</th>
        </tr>
      </thead>
      <tbody>
        {tickets.map((t) => (
          // The title is the link; the rest of the row is a larger target for the mouse.
          <tr key={t.id} onClick={() => go(ticketHref(t.number))}>
            <td className="number">{t.number}</td>
            <td><a href={ticketHref(t.number)}>{t.title}</a></td>
            <td>{t.customer_name}</td>
            <td>{t.requester_email ?? '—'}</td>
            <td>{t.assignee_name ?? '—'}</td>
            <td><span className={`state ${t.state}`}>{stateLabels[t.state]}</span></td>
            <td className="when">{formatTime(t.updated_at)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

export default App
