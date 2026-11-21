// The view shown, kept in location.hash so that reload and the back button work.
import { useSyncExternalStore } from 'react'
import type { TicketQuery, TicketState } from './api/client'

export const allStates: TicketState[] = ['open', 'waiting', 'resolved', 'closed']
/** What the ticket list shows when nothing else is asked for. */
export const notClosed: TicketState[] = ['open', 'waiting', 'resolved']

/** What the ticket list is narrowed to, as the agent picked it. */
export interface TicketFilters {
  /** Never empty, in the order of `allStates`. */
  states: TicketState[]
  /** '' for anyone, else `me`, `none` or an agent's id. */
  assignee: string
  /** '' for every customer, else a customer's id. */
  customer: string
  q: string
}

export const defaultFilters: TicketFilters = { states: notClosed, assignee: '', customer: '', q: '' }

export type Route =
  | { view: 'tickets'; filters: TicketFilters }
  | { view: 'inbox' }
  | { view: 'ticket'; number: number }
  | { view: 'customers' }
  | { view: 'customer'; id: string }
  | { view: 'services' }
  | { view: 'service'; id: string }

export const ticketHref = (number: number) => `#/tickets/${number}`
export const inboxHref = '#/inbox'
export const customersHref = '#/customers'
export const customerHref = (id: string) => `#/customers/${encodeURIComponent(id)}`
export const servicesHref = '#/services'
export const serviceHref = (id: string) => `#/services/${encodeURIComponent(id)}`

const sameStates = (a: TicketState[], b: TicketState[]) =>
  a.length === b.length && a.every((s) => b.includes(s))

/** The list's address; the default states are left out, so `#/tickets` is the default view. */
export function ticketsHref(filters: TicketFilters = defaultFilters): string {
  const parts: string[] = []
  if (!sameStates(filters.states, notClosed)) {
    parts.push(`state=${sameStates(filters.states, allStates) ? 'all' : filters.states.join(',')}`)
  }
  if (filters.assignee) parts.push(`assignee=${encodeURIComponent(filters.assignee)}`)
  if (filters.customer) parts.push(`customer=${encodeURIComponent(filters.customer)}`)
  if (filters.q) parts.push(`q=${encodeURIComponent(filters.q)}`)
  return parts.length ? `#/tickets?${parts.join('&')}` : '#/tickets'
}

/** What the core is asked for; every state is the same as not narrowing by state. */
export function ticketQuery(filters: TicketFilters): TicketQuery {
  return {
    state: sameStates(filters.states, allStates) ? undefined : filters.states.join(','),
    assignee: filters.assignee || undefined,
    customer: filters.customer || undefined,
    q: filters.q || undefined,
  }
}

function parseFilters(search: string): TicketFilters {
  const params = new URLSearchParams(search)
  const state = params.get('state')
  // Unknown states are dropped rather than sent: the core refuses them.
  const states = state === 'all'
    ? allStates
    : allStates.filter((s) => state?.split(',').map((p) => p.trim()).includes(s))
  return {
    states: states.length ? states : notClosed,
    assignee: params.get('assignee')?.trim() ?? '',
    customer: params.get('customer')?.trim() ?? '',
    q: params.get('q')?.trim() ?? '',
  }
}

export function parseRoute(hash: string): Route {
  if (hash === inboxHref) return { view: 'inbox' }
  if (hash === customersHref) return { view: 'customers' }
  if (hash === servicesHref) return { view: 'services' }
  const ticket = /^#\/tickets\/(\d+)$/.exec(hash)
  if (ticket) return { view: 'ticket', number: Number(ticket[1]) }
  const customer = /^#\/customers\/([^/?#]+)$/.exec(hash)
  if (customer) return { view: 'customer', id: decodeURIComponent(customer[1]) }
  const service = /^#\/services\/([^/?#]+)$/.exec(hash)
  if (service) return { view: 'service', id: decodeURIComponent(service[1]) }
  const tickets = /^#\/tickets\?(.*)$/.exec(hash)
  return { view: 'tickets', filters: tickets ? parseFilters(tickets[1]) : defaultFilters }
}

export function go(href: string) {
  window.location.hash = href
}

/** Shows `href` in place of the current address, without a new entry in the history. */
export function replace(href: string) {
  window.location.replace(href)
}

function subscribe(onChange: () => void) {
  window.addEventListener('hashchange', onChange)
  return () => window.removeEventListener('hashchange', onChange)
}

export function useRoute(): Route {
  const hash = useSyncExternalStore(subscribe, () => window.location.hash)
  // Parsed once per address, so that the filters keep their identity between renders.
  return routeOf(hash)
}

let cached: { hash: string; route: Route } | null = null
function routeOf(hash: string): Route {
  if (cached?.hash !== hash) cached = { hash, route: parseRoute(hash) }
  return cached.route
}
