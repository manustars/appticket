import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from 'react'
import { Account } from './Account'
import { Admin } from './Admin'
import {
  currentAgent,
  identityProviderSignIn,
  listInbox,
  listTickets,
  Refused,
  signIn,
  signInOptions,
  signOut,
  Unauthorized,
  type Agent,
  type InboxMessage,
  type TicketSummary,
} from './api/client'
import { CustomerPage, Customers } from './Customers'
import { useI18n } from './i18n/context'
import { LanguageSwitcher } from './i18n/LanguageSwitcher'
import { Import } from './Import'
import { Inbox } from './Inbox'
import {
  accountHref,
  adminHref,
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
import { adminPermissions, SessionContext, type Session } from './session'
import { Tickets } from './Tickets'
import { RequesterPage } from './Requester'
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
  return <Desk agent={agent} onAgent={setAgent} onSignedOut={signedOut} />
}

/** The same agent with the same role, as far as the screen shows it. */
const sameAgent = (a: Agent, b: Agent) => JSON.stringify(a) === JSON.stringify(b)

/** Why a sign-in through the identity provider did not open a session, as the core says it. */
const providerErrors = {
  not_in_authorized_group: 'signIn.refused.notInAuthorizedGroup',
  email_of_another_agent: 'signIn.refused.emailOfAnotherAgent',
  email_not_verified: 'signIn.refused.emailNotVerified',
  not_an_agent: 'signIn.refused.notAnAgent',
  deactivated: 'signIn.refused.deactivated',
  failed: 'signIn.refused.failed',
} as const
type ProviderError = keyof typeof providerErrors

/** The reason the core sent the browser back to the sign-in page with, if any. Only reads. */
function providerErrorInAddress(): ProviderError | null {
  const reason = new URLSearchParams(window.location.search).get('sign_in_error')
  if (reason === null) return null
  return reason in providerErrors ? (reason as ProviderError) : 'failed'
}

/** Takes the reason out of the address, so that a reload or a later sign-in page drops it. */
function dropProviderErrorFromAddress() {
  const params = new URLSearchParams(window.location.search)
  if (!params.has('sign_in_error')) return
  params.delete('sign_in_error')
  const query = params.toString()
  window.history.replaceState(null, '', window.location.pathname + (query ? `?${query}` : '') + window.location.hash)
}

type SignInOptions = Awaited<ReturnType<typeof signInOptions>>

function SignIn({ onSignedIn }: { onSignedIn: (agent: Agent) => void }) {
  const { t } = useI18n()
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [failed, setFailed] = useState<'wrong' | 'tooManyFailures' | 'localSignInOff' | 'unreachable' | null>(null)
  const [busy, setBusy] = useState(false)
  // Read while the address still has it: React may call this twice, which reads the same.
  const [providerError] = useState(providerErrorInAddress)
  // undefined: still asking the core; null: no answer, and the page offers the password only.
  const [options, setOptions] = useState<SignInOptions | null | undefined>(undefined)
  const provider = options?.identity_provider?.label ?? null
  const localSignIn = options === null || options?.local_sign_in === true

  const askOptions = useCallback(() => {
    signInOptions().then(setOptions, () => setOptions(null))
  }, [])

  useEffect(() => {
    dropProviderErrorFromAddress()
    askOptions()
  }, [askOptions])

  async function submit(event: FormEvent) {
    event.preventDefault()
    setBusy(true)
    setFailed(null)
    try {
      const outcome = await signIn(email, password)
      if (outcome === 'signedIn') {
        const agent = await currentAgent()
        if (agent) onSignedIn(agent)
      } else {
        setFailed(outcome)
        // Switched off since the page asked: it now offers what is left.
        if (outcome === 'localSignInOff') askOptions()
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
        {providerError && <p className="error" role="alert">{t(providerErrors[providerError])}</p>}
        {provider !== null && (
          <>
            <a className="button provider" href={identityProviderSignIn}>{t('signIn.with', { provider })}</a>
            {localSignIn && <p className="hint">{t('signIn.orPassword')}</p>}
          </>
        )}
        {options !== undefined && !localSignIn && provider === null && (
          <p className="error" role="alert">{t('signIn.noWayIn')}</p>
        )}
        {localSignIn && (
          <>
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
          </>
        )}
        {failed && (
          <p className="error" role="alert">
            {failed === 'wrong' ? t('signIn.wrong')
              : failed === 'tooManyFailures' ? t('signIn.tooManyFailures')
                : failed === 'localSignInOff' ? t('signIn.localSignInOff')
                  : t('error.unreachable')}
          </p>
        )}
        {localSignIn && <button type="submit" disabled={busy}>{t('signIn.submit')}</button>}
        <LanguageSwitcher />
      </form>
    </main>
  )
}

function Desk({ agent, onAgent, onSignedOut }: {
  agent: Agent
  /** The core answered with the agent again: their role may have changed meanwhile. */
  onAgent: (agent: Agent) => void
  onSignedOut: () => void
}) {
  const { t, formatNumber } = useI18n()
  const route = useRoute()
  // A change to the agent's role applies from their next request, so the screen asks again with
  // every refresh, and at once after a change made here.
  const refreshAgent = useCallback(async () => {
    const now = await currentAgent()
    if (now === null) onSignedOut()
    else if (!sameAgent(now, agent)) onAgent(now)
  }, [agent, onAgent, onSignedOut])
  const session = useMemo<Session>(() => ({
    agent,
    can: (permission) => agent.permissions.includes(permission),
    refresh: refreshAgent,
  }), [agent, refreshAgent])
  const { can } = session
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
    void refreshAgent().catch(() => {})
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
  }, [filters, onSignedOut, refreshAgent])

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
    : route.view === 'customer' || route.view === 'requester' || route.view === 'import' ? 'customers'
    : route.view === 'service' ? 'services'
    : route.view
  const current = (s: typeof section) => (section === s ? 'current' : '')

  return (
    <SessionContext value={session}>
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
            {adminPermissions.some(can) && (
              <button className={current('admin')} onClick={() => go(adminHref())}>
                {t('desk.admin')}
              </button>
            )}
          </nav>
          <a className={`who ${current('account')}`} href={accountHref} title={t('desk.account')}>{agent.name}</a>
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
          ) : route.view === 'import' ? (
            <Import onSignedOut={onSignedOut} />
          ) : route.view === 'customer' ? (
            <CustomerPage key={route.id} id={route.id} onChanged={refresh} onSignedOut={onSignedOut} />
          ) : route.view === 'requester' ? (
            <RequesterPage key={route.id} id={route.id} onChanged={refresh} onSignedOut={onSignedOut} />
          ) : route.view === 'services' ? (
            <Services onSignedOut={onSignedOut} />
          ) : route.view === 'service' ? (
            <ServicePage key={route.id} id={route.id} onSignedOut={onSignedOut} />
          ) : route.view === 'admin' ? (
            <Admin tab={route.tab} onSignedOut={onSignedOut} />
          ) : route.view === 'account' ? (
            <Account onSignedOut={onSignedOut} />
          ) : (
            <Tickets filters={route.filters} tickets={tickets} error={ticketsError} onSignedOut={onSignedOut} />
          )}
        </main>
      </div>
    </SessionContext>
  )
}

export default App
