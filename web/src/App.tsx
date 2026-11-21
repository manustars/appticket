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

const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: 'short', timeStyle: 'short' })

function App() {
  // undefined: still asking the core; null: nobody signed in.
  const [agent, setAgent] = useState<Agent | null | undefined>(undefined)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    currentAgent().then(setAgent, () => setError('The core is not reachable.'))
  }, [])

  if (error) return <main className="center"><p className="error">{error}</p></main>
  if (agent === undefined) return null
  if (agent === null) return <SignIn onSignedIn={setAgent} />
  return <Desk agent={agent} onSignedOut={() => setAgent(null)} />
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

type View = 'tickets' | 'inbox'

function Desk({ agent, onSignedOut }: { agent: Agent; onSignedOut: () => void }) {
  const [view, setView] = useState<View>('tickets')
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
          <button className={view === 'tickets' ? 'current' : ''} onClick={() => setView('tickets')}>
            Tickets {tickets && <span className="count">{tickets.length}</span>}
          </button>
          <button className={view === 'inbox' ? 'current' : ''} onClick={() => setView('inbox')}>
            Inbox {inbox && <span className="count">{inbox.length}</span>}
          </button>
        </nav>
        <span className="who">{agent.name}</span>
        <button className="quiet" onClick={leave}>Sign out</button>
      </header>
      <main>
        {error && <p className="error" role="alert">{error}</p>}
        {view === 'tickets' ? <Tickets tickets={tickets} /> : <Inbox messages={inbox} />}
      </main>
    </div>
  )
}

function Tickets({ tickets }: { tickets: TicketSummary[] | null }) {
  if (tickets === null) return null
  if (tickets.length === 0) return <p className="empty">No tickets yet.</p>
  return (
    <table>
      <thead>
        <tr><th>#</th><th>Title</th><th>Customer</th><th>Requester</th><th>State</th><th>Updated</th></tr>
      </thead>
      <tbody>
        {tickets.map((t) => (
          <tr key={t.id}>
            <td className="number">{t.number}</td>
            <td>{t.title}</td>
            <td>{t.customer_name}</td>
            <td>{t.requester_email ?? '—'}</td>
            <td><span className={`state ${t.state}`}>{t.state}</span></td>
            <td className="when">{dateTime.format(new Date(t.updated_at))}</td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

function Inbox({ messages }: { messages: InboxMessage[] | null }) {
  if (messages === null) return null
  if (messages.length === 0) return <p className="empty">The inbox is empty.</p>
  return (
    <ul className="inbox">
      {messages.map((m) => (
        <li key={m.id}>
          <div className="from">
            {m.from_name ? `${m.from_name} <${m.from_address}>` : m.from_address}
            <span className="when">{dateTime.format(new Date(m.received_at))}</span>
          </div>
          <div className="subject">{m.subject || '(no subject)'}</div>
          <p className="body">{m.body_text}</p>
        </li>
      ))}
    </ul>
  )
}

export default App
