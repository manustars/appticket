// Administration: the help desk's agents, its roles, its asset kinds, its add-ons and the state of
// its mail. Each tab needs a permission of its own, and only the tabs the agent's role allows are
// shown.
import { useId, useState, type FormEvent } from 'react'
import { AdminAddons } from './AdminAddons'
import { AdminKinds } from './AdminKinds'
import { AdminMail } from './AdminMail'
import { AdminRoles } from './AdminRoles'
import {
  addAgent,
  deactivateAgent,
  listAgentAccounts,
  listRoles,
  reactivateAgent,
  removeAgentPassword,
  resetAgentPassword,
  setAgentRole,
  unlinkAgentIdentity,
  type AgentAccount,
  type Role,
} from './api/client'
import { useI18n } from './i18n/context'
import { Problem } from './Problem'
import { adminHref, adminTabs, type AdminTab } from './route'
import { covers, roleLabel, useSession, type Session } from './session'
import { useAction, useLoad, type Action } from './useAction'

const tabPermission = {
  agents: 'agents.manage',
  roles: 'roles.manage',
  kinds: 'catalog.manage',
  addons: 'addons.manage',
  mail: 'mail.view',
} as const satisfies Record<AdminTab, string>

export function Admin({ tab, onSignedOut }: { tab: AdminTab | null; onSignedOut: () => void }) {
  const { t } = useI18n()
  const { can } = useSession()
  const allowed = adminTabs.filter((tab) => can(tabPermission[tab]))
  // `#/admin` shows the first tab the agent may see.
  const shown = tab ?? allowed[0] ?? null
  return (
    <div className="page">
      <h1>{t('admin.title')}</h1>
      {allowed.length > 0 && (
        <nav className="tabs" aria-label={t('admin.title')}>
          {allowed.map((tab) => (
            <a key={tab} href={adminHref(tab)} aria-current={tab === shown ? 'page' : undefined}>
              {t(`admin.${tab}`)}
            </a>
          ))}
        </nav>
      )}
      {shown === null || !allowed.includes(shown) ? (
        <p className="notice">{t('admin.notAllowed')}</p>
      ) : shown === 'agents' ? (
        <Agents onSignedOut={onSignedOut} />
      ) : shown === 'roles' ? (
        <AdminRoles onSignedOut={onSignedOut} />
      ) : shown === 'kinds' ? (
        <AdminKinds onSignedOut={onSignedOut} />
      ) : shown === 'addons' ? (
        <AdminAddons onSignedOut={onSignedOut} />
      ) : (
        <AdminMail onSignedOut={onSignedOut} />
      )}
    </div>
  )
}

const loadAgents = () => listAgentAccounts()
const loadRoles = () => listRoles()

/** A password the core generated, shown once to the agent who asked for it. */
interface Secret {
  kind: 'added' | 'reset'
  name: string
  email: string
  password: string
}

/** What the last change to an agent did, beyond what the list shows. */
type Done =
  | { kind: 'deactivated'; name: string; unassigned: number }
  | { kind: 'reactivated' | 'passwordRemoved' | 'unlinked'; name: string }

/** How the agent signs in: the key of its words. */
function signsInWith(a: AgentAccount) {
  if (a.has_local_password && a.has_linked_identity) return 'agents.signIn.both' as const
  if (a.has_local_password) return 'agents.signIn.localPassword' as const
  if (a.has_linked_identity) return 'agents.signIn.identityProvider' as const
  return 'agents.signIn.none' as const
}

/** Whether the agent of `session` can give `role`: only what their own role allows, and only an Administrator the Administrator role. */
function canGive(session: Session, role: Role): boolean {
  if (role.administrator) return session.agent.administrator
  return covers(session.agent.permissions, role.permissions)
}

function Agents({ onSignedOut }: { onSignedOut: () => void }) {
  const { t, formatTime } = useI18n()
  const session = useSession()
  const agents = useLoad(loadAgents, onSignedOut)
  const roles = useLoad(loadRoles, onSignedOut)
  const action = useAction(onSignedOut)
  // Kept above the list, so that reloading it after the change does not lose the password.
  const [secret, setSecret] = useState<Secret | null>(null)
  const [done, setDone] = useState<Done | null>(null)
  const id = useId()
  // Those who work at the help desk first, deactivated ones after them.
  const rows = agents.data
    ? [...agents.data.filter((a) => !a.deactivated_at), ...agents.data.filter((a) => a.deactivated_at)]
    : null

  /** Whether the signed-in agent may act on `a`: they hold every permission of `a`'s role. */
  function canActOn(a: AgentAccount): boolean {
    const role = roles.data?.find((r) => r.id === a.role_id)
    return role ? canGive(session, role) : false
  }

  async function changed(own: boolean) {
    await agents.reload()
    // A change to one's own role changes what this screen, and every other, may show.
    if (own) await session.refresh().catch(() => {})
  }

  function changeRole(a: AgentAccount, roleId: string) {
    const own = a.id === session.agent.id
    if (own && !window.confirm(t('agents.confirmOwnRole'))) return
    setDone(null)
    void action.run(() => setAgentRole(a.id, roleId)).then(() => changed(own))
  }

  function reset(a: AgentAccount) {
    if (!window.confirm(t('agents.confirmReset', { name: a.name }))) return
    setDone(null)
    let password = ''
    void action.run(async () => {
      password = await resetAgentPassword(a.id)
    }).then((ok) => {
      if (ok) setSecret({ kind: 'reset', name: a.name, email: a.email, password })
    })
  }

  function deactivate(a: AgentAccount) {
    if (!window.confirm(t('agents.confirmDeactivate', { name: a.name }))) return
    let unassigned = 0
    void action.run(async () => {
      unassigned = await deactivateAgent(a.id)
    }).then(async (ok) => {
      if (ok) setDone({ kind: 'deactivated', name: a.name, unassigned })
      await changed(false)
    })
  }

  function reactivate(a: AgentAccount) {
    void action.run(() => reactivateAgent(a.id)).then(async (ok) => {
      if (ok) setDone({ kind: 'reactivated', name: a.name })
      await changed(false)
    })
  }

  function removePassword(a: AgentAccount) {
    if (!window.confirm(t('agents.confirmRemovePassword', { name: a.name }))) return
    setDone(null)
    void action.run(() => removeAgentPassword(a.id)).then(async (ok) => {
      if (ok) setDone({ kind: 'passwordRemoved', name: a.name })
      await changed(false)
    })
  }

  function unlink(a: AgentAccount) {
    if (!window.confirm(t('agents.confirmUnlink', { name: a.name }))) return
    setDone(null)
    void action.run(() => unlinkAgentIdentity(a.id)).then(async (ok) => {
      if (ok) setDone({ kind: 'unlinked', name: a.name })
      await changed(false)
    })
  }

  return (
    <>
      {secret && <SecretNotice secret={secret} onDismiss={() => setSecret(null)} />}
      {done && (
        <p className="done" role="status">
          {done.kind === 'deactivated'
            ? t('agents.deactivated', { name: done.name, count: done.unassigned })
            : t(`agents.${done.kind}`, { name: done.name })}
        </p>
      )}
      <section className="panel" aria-labelledby={`${id}-title`}>
        <h2 id={`${id}-title`}>{t('agents.title')}</h2>
        <Problem error={agents.error ?? roles.error} />
        {rows && rows.length > 0 && (
          <table className="data agents">
            <thead>
              <tr>
                <th>{t('agents.name')}</th><th>{t('agents.email')}</th><th>{t('agents.role')}</th>
                <th>{t('agents.signsInWith')}</th><th>{t('agents.lastSignIn')}</th>
                <th><span className="visually-hidden">{t('table.actions')}</span></th>
              </tr>
            </thead>
            <tbody>
              {rows.map((a) => {
                const own = a.id === session.agent.id
                const may = canActOn(a)
                const givable = roles.data?.filter((r) => r.id === a.role_id || canGive(session, r)) ?? []
                return (
                  <tr key={a.id} className={a.deactivated_at ? 'retired' : ''}>
                    <td>
                      {a.name}
                      {own && <> <span className="marker">{t('agents.you')}</span></>}
                      {a.deactivated_at && (
                        <> <span className="marker retired">{t('agents.deactivatedOn', { date: formatTime(a.deactivated_at) })}</span></>
                      )}
                    </td>
                    <td>{a.email}</td>
                    <td>
                      {may && !a.deactivated_at && roles.data ? (
                        <select aria-label={t('agents.roleOf', { name: a.name })} value={a.role_id}
                          disabled={action.busy} onChange={(e) => changeRole(a, e.target.value)}>
                          {givable.map((r) => <option key={r.id} value={r.id}>{roleLabel(t, r)}</option>)}
                        </select>
                      ) : (
                        roleLabel(t, { administrator: a.administrator, name: a.role_name })
                      )}
                    </td>
                    <td>{t(signsInWith(a))}</td>
                    <td className="when">
                      {a.last_signed_in_at ? formatTime(a.last_signed_in_at) : <span className="muted">{t('agents.neverSignedIn')}</span>}
                    </td>
                    <td className="row-actions">
                      {/* One's own password is changed from one's account; one does not deactivate oneself. */}
                      {may && !own && !a.deactivated_at && (
                        <>
                          <button type="button" className="link" disabled={action.busy} onClick={() => reset(a)}>
                            {t('agents.resetPassword')}
                          </button>
                          {/* Either way in is taken away only from an agent who keeps the other. */}
                          {a.has_local_password && a.has_linked_identity && (
                            <>
                              <button type="button" className="link" disabled={action.busy} onClick={() => removePassword(a)}>
                                {t('agents.removePassword')}
                              </button>
                              <button type="button" className="link" disabled={action.busy} onClick={() => unlink(a)}>
                                {t('agents.unlinkIdentity')}
                              </button>
                            </>
                          )}
                          <button type="button" className="link danger" disabled={action.busy} onClick={() => deactivate(a)}>
                            {t('agents.deactivate')}
                          </button>
                        </>
                      )}
                      {may && a.deactivated_at && (
                        <button type="button" className="link" disabled={action.busy} onClick={() => reactivate(a)}>
                          {t('agents.reactivate')}
                        </button>
                      )}
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        )}
        <p className="hint">{t('agents.handOutHint')}</p>
        <p className="hint">{t('agents.signInHint')}</p>
        <Problem error={action.error} />
      </section>
      <AddAgent roles={roles.data} session={session} action={action}
        onAdded={async (added) => {
          setDone(null)
          setSecret(added)
          await agents.reload()
        }} />
    </>
  )
}

function AddAgent({ roles, session, action, onAdded }: {
  roles: Role[] | null
  session: Session
  /** Shared with the list, so that one change runs at a time. */
  action: Action
  onAdded: (secret: Secret) => Promise<void>
}) {
  const { t } = useI18n()
  const [email, setEmail] = useState('')
  const [name, setName] = useState('')
  // null until the agent picks: the default role, once the roles have arrived.
  const [chosen, setChosen] = useState<string | null>(null)
  const id = useId()
  const givable = roles?.filter((r) => canGive(session, r)) ?? []
  const roleId = chosen ?? givable.find((r) => r.default)?.id ?? ''

  function submit(event: FormEvent) {
    event.preventDefault()
    if (!email.trim() || !name.trim() || !roleId) return
    let password = ''
    const added = { email: email.trim(), name: name.trim() }
    void action.run(async () => {
      password = (await addAgent(added.email, added.name, roleId)).password
    }).then(async (ok) => {
      if (!ok) return
      setEmail('')
      setName('')
      setChosen(null)
      await onAdded({ kind: 'added', ...added, password })
    })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('agents.add')}</h2>
      <form className="inline-form" onSubmit={submit}>
        <label>
          {t('agents.email')}
          <input type="email" required value={email} placeholder={t('agents.emailPlaceholder')}
            onChange={(e) => setEmail(e.target.value)} />
        </label>
        <label>
          {t('agents.name')}
          <input required value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label>
          {t('agents.role')}
          <select required value={roleId} disabled={!roles} onChange={(e) => setChosen(e.target.value)}>
            {!roles && <option value="">{t('agents.loadingRoles')}</option>}
            {roles && !roleId && <option value="">{t('agents.chooseRole')}</option>}
            {givable.map((r) => (
              <option key={r.id} value={r.id}>
                {r.default ? t('agents.defaultRole', { name: roleLabel(t, r) }) : roleLabel(t, r)}
              </option>
            ))}
          </select>
        </label>
        <button type="submit" disabled={action.busy || !email.trim() || !name.trim() || !roleId}>
          {t('agents.addSubmit')}
        </button>
      </form>
      <p className="hint">{t('agents.addHint')}</p>
    </section>
  )
}

/** The generated password, with a way to copy it; it is never shown again. */
function SecretNotice({ secret, onDismiss }: { secret: Secret; onDismiss: () => void }) {
  const { t } = useI18n()
  const [copied, setCopied] = useState(false)

  function copy() {
    // The clipboard is there only on a secure origin; the password can still be selected by hand.
    navigator.clipboard?.writeText(secret.password).then(() => setCopied(true), () => setCopied(false))
  }

  return (
    <section className="secret" role="status">
      <p>
        {secret.kind === 'added'
          ? t('agents.addedPassword', { name: secret.name, email: secret.email })
          : t('agents.resetDone', { name: secret.name, email: secret.email })}
      </p>
      <div className="secret-row">
        <code>{secret.password}</code>
        <button type="button" onClick={copy}>{copied ? t('agents.copied') : t('agents.copy')}</button>
      </div>
      <p className="secret-warning">{t('agents.shownOnce')}</p>
      <div>
        <button type="button" className="quiet" onClick={onDismiss}>{t('agents.dismiss')}</button>
      </div>
    </section>
  )
}
