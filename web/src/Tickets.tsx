import { useCallback, useEffect, useRef, useState } from 'react'
import {
  getCustomer,
  listAgents,
  listAllCustomers,
  Refused,
  type AgentEntry,
  type CustomerDetail,
  type CustomerSummary,
  type TicketState,
  type TicketSummary,
} from './api/client'
import { DepartedMarker } from './DepartedMarker'
import { useI18n } from './i18n/context'
import { NewTicket } from './NewTicket'
import {
  allStates,
  defaultFilters,
  go,
  notClosed,
  parseRoute,
  replace,
  ticketHref,
  ticketsHref,
  type TicketFilters,
} from './route'
import { useSession } from './session'
import { useLoad } from './useAction'

/** How long typing pauses before the search is sent. */
const searchDelay = 300

interface Props {
  filters: TicketFilters
  /** null until the first answer for these filters arrives. */
  tickets: TicketSummary[] | null
  /** Why the list could not be loaded, or null. */
  error: unknown
  onSignedOut: () => void
}

export function Tickets({ filters, tickets, error, onSignedOut }: Props) {
  const { t } = useI18n()
  const filtered = ticketsHref(filters) !== ticketsHref({ ...defaultFilters, states: allStates })
  const [opening, setOpening] = useState(false)
  // Opening tickets by hand is a permission of its own; reading the list is not.
  const canOpen = useSession().can('tickets.open')
  return (
    <div className="tickets-screen">
      <div className="tickets-top">
        <TicketFilterBar filters={filters} onSignedOut={onSignedOut} />
        {canOpen && !opening && (
          <button type="button" className="new-ticket" onClick={() => setOpening(true)}>{t('tickets.new')}</button>
        )}
      </div>
      {canOpen && opening && (
        // A customer picked in the filter is the likely one for the new ticket.
        <NewTicket customerId={filters.customer} onCancel={() => setOpening(false)} onSignedOut={onSignedOut} />
      )}
      {error !== null && (
        <p className="error" role="alert">
          {error instanceof Refused ? error.message : t('error.loadFailed')}
        </p>
      )}
      {tickets !== null && tickets.length === 0 && (
        <p className="empty">{filtered ? t('tickets.noMatch') : t('tickets.empty')}</p>
      )}
      {tickets !== null && tickets.length > 0 && <TicketTable tickets={tickets} />}
    </div>
  )
}

/** Tickets as rows, each a link to its ticket. */
export function TicketTable({ tickets }: { tickets: TicketSummary[] }) {
  const { t, formatTime } = useI18n()
  return (
    <table className="tickets">
      <thead>
        <tr>
          <th>{t('tickets.number')}</th><th>{t('tickets.title')}</th><th>{t('tickets.customer')}</th>
          <th>{t('tickets.sites')}</th><th>{t('tickets.requester')}</th><th>{t('tickets.assignee')}</th><th>{t('tickets.state')}</th>
          <th>{t('tickets.priority')}</th>
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
            <td>{ticket.sites.length > 0 ? ticket.sites.map((s) => s.name).join(', ') : '—'}</td>
            <td>
              {ticket.requester_email ?? '—'}
              {ticket.requester_departed && <DepartedMarker />}
            </td>
            <td>{ticket.assignee_name ?? '—'}</td>
            <td><span className={`state ${ticket.state}`}>{t(`state.${ticket.state}`)}</span></td>
            <td><span className={`priority ${ticket.priority}`}>{t(`priority.${ticket.priority}`)}</span></td>
            <td className="when">{formatTime(ticket.updated_at)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

type StateChoice = 'all' | 'notClosed' | TicketState | 'custom'

function stateChoice(states: TicketState[]): StateChoice {
  if (states.length === allStates.length) return 'all'
  if (states.length === notClosed.length && notClosed.every((s) => states.includes(s))) return 'notClosed'
  if (states.length === 1) return states[0]
  return 'custom'
}

function statesOf(choice: Exclude<StateChoice, 'custom'>): TicketState[] {
  if (choice === 'all') return allStates
  if (choice === 'notClosed') return notClosed
  return [choice]
}

/** The filters in the address now, which may be newer than the ones this render saw. */
function currentFilters(): TicketFilters {
  const route = parseRoute(window.location.hash)
  return route.view === 'tickets' ? route.filters : defaultFilters
}

/**
 * Changes the address in place: going back leaves the list rather than undoing each filter, and
 * the list it returns to keeps its filters.
 */
function setFilters(change: Partial<TicketFilters>) {
  replace(ticketsHref({ ...currentFilters(), ...change }))
}

const loadAgents = () => listAgents()
const loadCustomers = () => listAllCustomers()

function TicketFilterBar({ filters, onSignedOut }: { filters: TicketFilters; onSignedOut: () => void }) {
  const { t } = useI18n()
  const agents = useLoad<AgentEntry[]>(loadAgents, onSignedOut)
  const customers = useLoad<CustomerSummary[]>(loadCustomers, onSignedOut)
  // A site is picked among the chosen customer's: site names are only unique within a customer.
  const loadSites = useCallback(
    () => (filters.customer ? getCustomer(filters.customer).then((c) => c.sites) : Promise.resolve([])),
    [filters.customer])
  const sites = useLoad<CustomerDetail['sites']>(loadSites, onSignedOut)
  const choice = stateChoice(filters.states)
  const isDefault = ticketsHref(filters) === ticketsHref(defaultFilters)

  // The search box keeps what is typed; the address follows once typing pauses.
  const [text, setText] = useState(filters.q)
  const [shownQ, setShownQ] = useState(filters.q)
  if (filters.q !== shownQ) {
    // The address changed from elsewhere (reset, back button): show what it asks for.
    setShownQ(filters.q)
    if (filters.q !== text.trim()) setText(filters.q)
  }
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)
  useEffect(() => () => clearTimeout(timer.current), [])
  const search = useCallback((value: string) => {
    setText(value)
    clearTimeout(timer.current)
    timer.current = setTimeout(() => setFilters({ q: value.trim() }), searchDelay)
  }, [])

  // Offboarded customers keep their history, so they can be picked too, apart from the others.
  const active = customers.data?.filter((c) => !c.offboarded_at) ?? []
  const offboarded = customers.data?.filter((c) => c.offboarded_at) ?? []
  const knownCustomer = !filters.customer || customers.data?.some((c) => c.id === filters.customer)
  // Retired sites keep the tickets closed at them, so they can be picked too, apart from the others.
  const activeSites = sites.data?.filter((s) => !s.retired_at) ?? []
  const retiredSites = sites.data?.filter((s) => s.retired_at) ?? []
  const knownSite = !filters.site || sites.data?.some((s) => s.id === filters.site)
  const knownAssignee = ['', 'me', 'none'].includes(filters.assignee)
    || agents.data?.some((a) => a.id === filters.assignee)

  return (
    <form className="filters" role="search" aria-label={t('filters.label')}
      onSubmit={(e) => {
        e.preventDefault()
        clearTimeout(timer.current)
        setFilters({ q: text.trim() })
      }}>
      <label>
        {t('filters.state')}
        <select value={choice}
          onChange={(e) => setFilters({ states: statesOf(e.target.value as Exclude<StateChoice, 'custom'>) })}>
          <option value="notClosed">{t('filters.notClosed')}</option>
          <option value="all">{t('filters.allStates')}</option>
          {allStates.map((s) => <option key={s} value={s}>{t(`state.${s}`)}</option>)}
          {choice === 'custom' && (
            <option value="custom" disabled>{filters.states.map((s) => t(`state.${s}`)).join(', ')}</option>
          )}
        </select>
      </label>
      <label>
        {t('filters.assignee')}
        <select value={filters.assignee} onChange={(e) => setFilters({ assignee: e.target.value })}>
          <option value="">{t('filters.anyone')}</option>
          <option value="me">{t('filters.me')}</option>
          <option value="none">{t('filters.unassigned')}</option>
          {agents.data?.map((a) => <option key={a.id} value={a.id}>{a.name}</option>)}
          {/* An address typed by hand keeps showing what it asks for. */}
          {!knownAssignee && <option value={filters.assignee}>{filters.assignee}</option>}
        </select>
      </label>
      <label>
        {t('filters.customer')}
        <select value={filters.customer} onChange={(e) => setFilters({ customer: e.target.value, site: '' })}>
          <option value="">{t('filters.allCustomers')}</option>
          {active.map((c) => (
            <option key={c.id} value={c.id}>{c.is_help_desk ? t('customer.helpDesk', { name: c.name }) : c.name}</option>
          ))}
          {offboarded.length > 0 && (
            <optgroup label={t('filters.offboarded')}>
              {offboarded.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
            </optgroup>
          )}
          {!knownCustomer && customers.data && <option value={filters.customer}>{filters.customer}</option>}
        </select>
      </label>
      {(filters.customer || filters.site) && (
        <label>
          {t('filters.site')}
          <select value={filters.site} onChange={(e) => setFilters({ site: e.target.value })}>
            <option value="">{t('filters.allSites')}</option>
            {activeSites.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
            {retiredSites.length > 0 && (
              <optgroup label={t('filters.retiredSites')}>
                {retiredSites.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
              </optgroup>
            )}
            {!knownSite && sites.data && <option value={filters.site}>{filters.site}</option>}
          </select>
        </label>
      )}
      <label className="search">
        {t('filters.search')}
        <input type="search" value={text} placeholder={t('filters.searchPlaceholder')}
          onChange={(e) => search(e.target.value)} />
      </label>
      {!isDefault && (
        <button type="button" className="quiet" onClick={() => {
          clearTimeout(timer.current)
          replace(ticketsHref(defaultFilters))
        }}>{t('filters.reset')}</button>
      )}
    </form>
  )
}
