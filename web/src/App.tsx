import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react'
import {
  currentAgent,
  listInbox,
  listTickets,
  Refused,
  signIn,
  signOut,
  Unauthorized,
  type Agent,
  type InboxMessage,
  type TicketSummary,
} from './api/client'
import { CustomerPage, Customers } from './Customers'
import { useI18n } from './i18n/context'
import { LanguageSwitcher } from './i18n/LanguageSwitcher'
import { Inbox } from './Inbox'
import {
  customersHref,
  defaultFilters,
  go,
  inboxHref,
  servicesHref,
  ticketQuery,
  ticketsHref,
  useRoute,
  type TicketFilters,
} from './route'
import { ServicePage, Services } from './Services'
import { Tickets } from './Tickets'
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
  // The filters of the list last shown, kept while a ticket or another screen is open, so that the
  // way back finds the list as it was.
  const [filters, setFilters] = useState<TicketFilters>(
    route.view === 'tickets' ? route.filters : defaultFilters)
  if (route.view === 'tickets' && ticketsHref(route.filters) !== ticketsHref(filters)) {
    setFilters(route.filters)
  }
  const listHref = ticketsHref(filters)
  const [tickets, setTickets] = useState<TicketSummary[] | null>(null)
  const [ticketsError, setTicketsError] = useState<unknown>(null)
  const [inbox, setInbox] = useState<InboxMessage[] | null>(null)
  const [inboxFailed, setInboxFailed] = useState(false)
  // Only the latest list may be shown: an answer for filters changed since must not overwrite it.
  const latest = useRef(0)

  const refresh = useCallback(async () => {
    const mine = ++latest.current
    const [list, waiting] = await Promise.allSettled([listTickets(ticketQuery(filters)), listInbox()])
    if ([list, waiting].some((r) => r.status === 'rejected' && r.reason instanceof Unauthorized)) {
      onSignedOut()
      return
    }
    if (mine === latest.current) {
      if (list.status === 'fulfilled') {
        setTickets(list.value)
        setTicketsError(null)
      } else {
        // A refused filter shows the core's words instead of a list it did not give.
        if (list.reason instanceof Refused) setTickets(null)
        setTicketsError(list.reason)
      }
    }
    if (waiting.status === 'fulfilled') {
      setInbox(waiting.value)
      setInboxFailed(false)
    } else {
      setInboxFailed(true)
    }
  }, [filters, onSignedOut])

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

  const section = route.view === 'ticket' ? 'tickets'
    : route.view === 'customer' ? 'customers'
    : route.view === 'service' ? 'services'
    : route.view
  const current = (s: typeof section) => (section === s ? 'current' : '')

  return (
    <div className="desk">
      <header>
        <nav>
          {/* The count is of the list as filtered, as the list itself shows it. */}
          <button className={current('tickets')} onClick={() => go(listHref)}>
            {t('desk.tickets')} {tickets && <span className="count">{formatNumber(tickets.length)}</span>}
          </button>
          <button className={current('inbox')} onClick={() => go(inboxHref)}>
            {t('desk.inbox')} {inbox && <span className="count">{formatNumber(inbox.length)}</span>}
          </button>
          <button className={current('customers')} onClick={() => go(customersHref)}>
            {t('desk.customers')}
          </button>
          <button className={current('services')} onClick={() => go(servicesHref)}>
            {t('desk.services')}
          </button>
        </nav>
        <span className="who">{agent.name}</span>
        <LanguageSwitcher />
        <button className="quiet" onClick={leave}>{t('desk.signOut')}</button>
      </header>
      <main>
        {inboxFailed && <p className="error" role="alert">{t('error.loadFailed')}</p>}
        {route.view === 'ticket' ? (
          <TicketView key={route.number} number={route.number} backHref={listHref} onChanged={refresh}
            onSignedOut={onSignedOut} />
        ) : route.view === 'inbox' ? (
          <Inbox messages={inbox} onChanged={refresh} onSignedOut={onSignedOut} />
        ) : route.view === 'customers' ? (
          <Customers onSignedOut={onSignedOut} />
        ) : route.view === 'customer' ? (
          <CustomerPage key={route.id} id={route.id} onChanged={refresh} onSignedOut={onSignedOut} />
        ) : route.view === 'services' ? (
          <Services onSignedOut={onSignedOut} />
        ) : route.view === 'service' ? (
          <ServicePage key={route.id} id={route.id} onSignedOut={onSignedOut} />
        ) : (
          <Tickets filters={route.filters} tickets={tickets} error={ticketsError} onSignedOut={onSignedOut} />
        )}
      </main>
    </div>
  )
}

export default App
