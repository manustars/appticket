// The view shown, kept in location.hash so that reload and the back button work.
import { useSyncExternalStore } from 'react'

export type Route =
  | { view: 'tickets' }
  | { view: 'inbox' }
  | { view: 'ticket'; number: number }

export const ticketHref = (number: number) => `#/tickets/${number}`
export const ticketsHref = '#/tickets'
export const inboxHref = '#/inbox'

export function parseRoute(hash: string): Route {
  if (hash === inboxHref) return { view: 'inbox' }
  const ticket = /^#\/tickets\/(\d+)$/.exec(hash)
  if (ticket) return { view: 'ticket', number: Number(ticket[1]) }
  return { view: 'tickets' }
}

export function go(href: string) {
  window.location.hash = href
}

function subscribe(onChange: () => void) {
  window.addEventListener('hashchange', onChange)
  return () => window.removeEventListener('hashchange', onChange)
}

export function useRoute(): Route {
  const hash = useSyncExternalStore(subscribe, () => window.location.hash)
  return parseRoute(hash)
}
