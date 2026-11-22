// The signed-in agent and what their role allows, for every screen to show only what it can do.
// The core decides: hiding an action here is a courtesy, and a refusal is still shown if it comes.
import { createContext, useContext } from 'react'
import type { Agent, Permission } from './api/client'
import type { I18n } from './i18n/translate'

export interface Session {
  agent: Agent
  /** Whether the agent's role allows `permission`; an Administrator's allows them all. */
  can: (permission: Permission) => boolean
  /** Asks the core again, after a change that may touch the agent's own role. */
  refresh: () => Promise<void>
}

export const SessionContext = createContext<Session | null>(null)

export function useSession(): Session {
  const session = useContext(SessionContext)
  if (!session) throw new Error('useSession needs a SessionContext above it')
  return session
}

/** The permissions behind the administration screens, in the order of their tabs. */
export const adminPermissions = ['agents.manage', 'roles.manage', 'addons.manage', 'mail.view'] as const

/** Whether one role's permissions are all among another's: what the agent can hand out. */
export const covers = (held: readonly Permission[], wanted: readonly Permission[]) =>
  wanted.every((p) => held.includes(p))

/** A role's name as the screen shows it: the Administrator role's in the agent's language. */
export const roleLabel = (t: I18n['t'], role: { administrator: boolean; name: string }) =>
  role.administrator ? t('role.administrator') : role.name
