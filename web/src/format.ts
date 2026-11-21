// Shared wording and formatting of the agents' UI.
import { Refused, type TicketState } from './api/client'

export const dateTime = new Intl.DateTimeFormat(undefined, { dateStyle: 'short', timeStyle: 'short' })

export const formatTime = (iso: string) => dateTime.format(new Date(iso))

export const stateLabels: Record<TicketState, string> = {
  open: 'Open',
  waiting: 'Waiting',
  resolved: 'Resolved',
  closed: 'Closed',
}

/** What to tell the agent about a failed call; the core's own text when it refused. */
export function errorText(error: unknown): string {
  if (error instanceof Refused) return error.message
  return 'The core is not reachable.'
}
