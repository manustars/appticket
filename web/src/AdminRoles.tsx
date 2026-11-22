// The help desk's roles: named sets of the permissions the product defines. The Administrator role
// is built in, allows everything and is neither changed nor removed.
import { useId, useState, type FormEvent } from 'react'
import {
  createRole,
  listPermissions,
  listRoles,
  makeDefaultRole,
  removeRole,
  updateRole,
  type Permission,
  type Role,
} from './api/client'
import { useI18n } from './i18n/context'
import { Problem } from './Problem'
import { covers, roleLabel, useSession } from './session'
import { useAction, useLoad } from './useAction'

const loadPermissions = () => listPermissions()
const loadRoles = () => listRoles()

export function AdminRoles({ onSignedOut }: { onSignedOut: () => void }) {
  const { t } = useI18n()
  const session = useSession()
  const permissions = useLoad(loadPermissions, onSignedOut)
  const roles = useLoad(loadRoles, onSignedOut)
  const { reload } = roles

  async function changed() {
    await reload()
    // The agent's own role may be among those changed.
    await session.refresh().catch(() => {})
  }

  return (
    <>
      <p className="hint">{t('roles.hint')}</p>
      <Problem error={permissions.error ?? roles.error} />
      {roles.data && permissions.data && (
        <>
          {roles.data.map((r) => (
            r.administrator
              ? <AdministratorRole key={r.id} role={r} />
              : <RoleEditor key={`${r.id}-${r.name}-${r.permissions.join()}`} role={r} all={permissions.data ?? []}
                onChanged={changed} onSignedOut={onSignedOut} />
          ))}
          <NewRole all={permissions.data} onChanged={changed} onSignedOut={onSignedOut} />
        </>
      )}
    </>
  )
}

function RoleFacts({ role: r }: { role: Role }) {
  const { t } = useI18n()
  return (
    <>
      {r.default && <span className="marker">{t('roles.default')}</span>}
      <span className="muted">{t('roles.agents', { count: r.agents })}</span>
    </>
  )
}

function AdministratorRole({ role: r }: { role: Role }) {
  const { t } = useI18n()
  const id = useId()
  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <div className="title-row">
        <h2 id={`${id}-title`}>{roleLabel(t, r)}</h2>
        <span className="marker">{t('roles.everyPermission')}</span>
        <RoleFacts role={r} />
      </div>
      <p className="hint">{t('roles.administratorHint')}</p>
    </section>
  )
}

/** The permissions as checkboxes, each with what it allows; `held` are those the agent can hand out. */
function PermissionChecks({ all, value, held, disabled, onChange }: {
  all: Permission[]
  value: Permission[]
  held: Permission[]
  disabled: boolean
  onChange: (value: Permission[]) => void
}) {
  const { t } = useI18n()
  return (
    <ul className="permissions">
      {all.map((p) => (
        <li key={p}>
          <label>
            <input type="checkbox" checked={value.includes(p)} disabled={disabled || !held.includes(p)}
              onChange={(e) => onChange(e.target.checked ? all.filter((q) => q === p || value.includes(q))
                : value.filter((q) => q !== p))} />
            <span>
              <strong>{t(`permission.${p}`)}</strong>
              <span className="hint">{t(`permissionHint.${p}`)}</span>
            </span>
          </label>
        </li>
      ))}
    </ul>
  )
}

const same = (a: Permission[], b: Permission[]) => a.length === b.length && a.every((p) => b.includes(p))

function RoleEditor({ role: r, all, onChanged, onSignedOut }: {
  role: Role
  all: Permission[]
  onChanged: () => Promise<void>
  onSignedOut: () => void
}) {
  const { t } = useI18n()
  const { agent } = useSession()
  const action = useAction(onSignedOut)
  const [name, setName] = useState(r.name)
  const [chosen, setChosen] = useState<Permission[]>(r.permissions)
  const id = useId()
  // A role is changed only by an agent who holds every permission it has.
  const editable = covers(agent.permissions, r.permissions)
  const dirty = name.trim() !== r.name || !same(chosen, r.permissions)
  // The core refuses these too; said here before the agent tries.
  const removable = !r.default && r.agents === 0

  function save(event: FormEvent) {
    event.preventDefault()
    if (!name.trim()) return
    void action.run(() => updateRole(r.id, name.trim(), chosen)).then(async (ok) => { if (ok) await onChanged() })
  }

  function makeDefault() {
    void action.run(() => makeDefaultRole(r.id)).then(async (ok) => { if (ok) await onChanged() })
  }

  function remove() {
    if (!window.confirm(t('roles.confirmRemove', { name: r.name }))) return
    void action.run(() => removeRole(r.id)).then(async (ok) => { if (ok) await onChanged() })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <div className="title-row">
        <h2 id={`${id}-title`}>{r.name}</h2>
        <RoleFacts role={r} />
      </div>
      {!editable && <p className="hint">{t('roles.notYours')}</p>}
      <form className="stacked" onSubmit={save}>
        <label className="role-name">
          {t('roles.name')}
          <input required value={name} disabled={!editable} onChange={(e) => setName(e.target.value)} />
        </label>
        <PermissionChecks all={all} value={chosen} held={agent.permissions} disabled={!editable || action.busy}
          onChange={setChosen} />
        <Problem error={action.error} />
        {editable && (
          <div className="role-actions">
            <button type="submit" disabled={action.busy || !dirty || !name.trim()}>{t('action.save')}</button>
            {dirty && (
              <button type="button" className="quiet" onClick={() => { setName(r.name); setChosen(r.permissions); action.clear() }}>
                {t('action.cancel')}
              </button>
            )}
            {!r.default && (
              <button type="button" disabled={action.busy || dirty} onClick={makeDefault}>{t('roles.makeDefault')}</button>
            )}
            <button type="button" className="danger" disabled={action.busy || !removable} onClick={remove}
              aria-describedby={removable ? undefined : `${id}-why`}>
              {t('roles.remove')}
            </button>
            {!removable && (
              <span className="hint" id={`${id}-why`}>
                {r.default ? t('roles.defaultNotRemovable') : t('roles.inUse')}
              </span>
            )}
          </div>
        )}
      </form>
    </section>
  )
}

function NewRole({ all, onChanged, onSignedOut }: {
  all: Permission[]
  onChanged: () => Promise<void>
  onSignedOut: () => void
}) {
  const { t } = useI18n()
  const { agent } = useSession()
  const action = useAction(onSignedOut)
  const [name, setName] = useState('')
  const [chosen, setChosen] = useState<Permission[]>([])
  const id = useId()

  function submit(event: FormEvent) {
    event.preventDefault()
    if (!name.trim()) return
    void action.run(async () => {
      await createRole(name.trim(), chosen)
    }).then(async (ok) => {
      if (!ok) return
      setName('')
      setChosen([])
      await onChanged()
    })
  }

  return (
    <section className="panel" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('roles.add')}</h2>
      <form className="stacked" onSubmit={submit}>
        <label className="role-name">
          {t('roles.name')}
          <input required value={name} placeholder={t('roles.namePlaceholder')} onChange={(e) => setName(e.target.value)} />
        </label>
        <PermissionChecks all={all} value={chosen} held={agent.permissions} disabled={action.busy} onChange={setChosen} />
        <Problem error={action.error} />
        <div className="role-actions">
          <button type="submit" disabled={action.busy || !name.trim()}>{t('roles.addSubmit')}</button>
        </div>
      </form>
    </section>
  )
}
