// Proposals of add-ons such as the Assistant: suggestions an agent accepts or discards. They are
// marked as suggestions and carry the add-on's reason, so they never pass for the agent's own work.
import { useId, type ReactNode } from 'react'
import { addonName, inLanguage } from './addon'
import { acceptProposal, discardProposal, type Permission, type Proposal, type TicketDetail } from './api/client'
import { useI18n } from './i18n/context'
import { Problem } from './Problem'
import { useSession } from './session'
import { useAction } from './useAction'

/**
 * The permission deciding on a proposal needs, as the core checks it: sorting the inbox for
 * triage, replying to accept a draft reply, working tickets otherwise.
 */
function needs(proposal: Proposal, accepting: boolean): Permission {
  if (proposal.content.kind === 'triage') return 'inbox.triage'
  if (proposal.content.kind === 'reply' && accepting) return 'tickets.reply'
  return 'tickets.work'
}

/** The quiet mark that says: an add-on suggests this, nothing has happened yet. */
export function SuggestionTag({ addon }: { addon?: string }) {
  const { t } = useI18n()
  return (
    <span className="suggestion-tag">
      {addon ? t('proposals.suggestedBy', { addon: addonName(addon) }) : t('proposals.suggestion')}
    </span>
  )
}

/** What a proposal would do, in words. */
export function ProposalWhat({ proposal: p }: { proposal: Proposal }) {
  const { t } = useI18n()
  const c = p.content
  switch (c.kind) {
    case 'triage':
      if (c.action === 'open_ticket') {
        return <>{t('proposals.openTicket', { customer: p.customer_name ?? t('proposals.someCustomer') })}</>
      }
      if (c.action === 'attach') return <>{t('proposals.attach', { number: c.ticket_number })}</>
      return <>{t('proposals.discardMessage')}</>
    case 'priority':
      return <>{t('proposals.priority', { priority: t(`priority.${c.priority}`) })}</>
    case 'assets':
      return <>{t('proposals.assets', { assets: p.assets.map((a) => `${a.name} (${a.kind})`).join(', ') })}</>
    case 'services':
      return <>{t('proposals.services', { services: p.services.map((s) => s.name).join(', ') })}</>
    case 'reply':
      return (
        <>
          {t('proposals.reply')}
          <blockquote className="draft">{c.body}</blockquote>
        </>
      )
  }
}

/** One proposal with its reason and the choices the agent's role allows. */
function ProposalItem({ proposal, busy, onAccept, onDiscard, extra }: {
  proposal: Proposal
  busy: boolean
  onAccept: () => void
  onDiscard: () => void
  extra?: ReactNode
}) {
  const { t, locale } = useI18n()
  const { can } = useSession()
  const canAccept = can(needs(proposal, true))
  const canDiscard = can(needs(proposal, false))
  // Follows the language the agent picks, as the add-on wrote it for that language.
  const rationale = inLanguage(proposal.rationale, locale)
  return (
    <li className="proposal">
      <div className="proposal-what"><ProposalWhat proposal={proposal} /></div>
      {rationale && <p className="rationale">{rationale}</p>}
      {extra}
      {(canAccept || canDiscard) && (
        <div className="proposal-actions">
          {canAccept && <button type="button" disabled={busy} onClick={onAccept}>{t('proposals.accept')}</button>}
          {canDiscard && (
            <button type="button" className="quiet" disabled={busy} onClick={onDiscard}>{t('proposals.discard')}</button>
          )}
        </div>
      )}
    </li>
  )
}

interface TicketProps {
  ticket: TicketDetail
  /** Whether the composer holds text the agent typed; accepting a draft reply asks before replacing it. */
  draftInUse: boolean
  /** An accepted draft reply, for the composer. */
  onReplyDraft: (addon: string, body: string) => void
  /** Called after a change: reloads the ticket and the lists. */
  onChanged: () => Promise<void>
  onSignedOut: () => void
}

/** The open proposals on a ticket, one panel per add-on. */
export function TicketProposals({ ticket, ...rest }: TicketProps) {
  const addons = [...new Set(ticket.proposals.map((p) => p.addon))]
  return (
    <>
      {addons.map((addon) => (
        <AddonProposals key={addon} addon={addon} ticket={ticket}
          proposals={ticket.proposals.filter((p) => p.addon === addon)} {...rest} />
      ))}
    </>
  )
}

function AddonProposals({ addon, proposals, draftInUse, onReplyDraft, onChanged, onSignedOut }: TicketProps & {
  addon: string
  proposals: Proposal[]
}) {
  const { t } = useI18n()
  const action = useAction(onSignedOut)
  const id = useId()
  const name = addonName(addon)

  function accept(p: Proposal) {
    if (p.content.kind === 'reply' && draftInUse && !window.confirm(t('proposals.replaceDraft', { addon: name }))) return
    let body: string | null = null
    void action.run(async () => {
      body = (await acceptProposal(p.id)).reply_body ?? null
    }).then(async () => {
      if (body !== null) onReplyDraft(addon, body)
      await onChanged()
    })
  }

  function discard(p: Proposal) {
    void action.run(() => discardProposal(p.id)).then(() => onChanged())
  }

  return (
    <section className="panel proposals" aria-labelledby={`${id}-title`}>
      <div className="proposals-head">
        <h2 id={`${id}-title`}>{t('proposals.title', { addon: name })}</h2>
        <SuggestionTag />
      </div>
      <p className="hint">{t('proposals.hint', { addon: name })}</p>
      <ul className="proposal-list">
        {proposals.map((p) => (
          <ProposalItem key={p.id} proposal={p} busy={action.busy}
            onAccept={() => accept(p)} onDiscard={() => discard(p)} />
        ))}
      </ul>
      <Problem error={action.error} />
    </section>
  )
}

/** What accepting a triage proposal did, for the inbox's notice. */
export type TriageDone =
  | { kind: 'opened'; ticket: number }
  | { kind: 'attached'; ticket: number }
  | { kind: 'discarded' }

/** The open triage proposal on an inbox message. */
export function InboxProposal({ proposal: p, sender, onDone, onDiscarded, onSignedOut }: {
  proposal: Proposal
  /** The message's sender, for the confirmation before a discard. */
  sender: string
  onDone: (done: TriageDone) => void
  /** The proposal was discarded; the message stays. */
  onDiscarded: () => void
  onSignedOut: () => void
}) {
  const { t } = useI18n()
  const action = useAction(onSignedOut)
  const c = p.content

  function accept() {
    if (c.kind !== 'triage') return
    // Discarding a message is confirmed whoever suggests it.
    if (c.action === 'discard' && !window.confirm(t('inbox.confirmDiscard', { sender }))) return
    let number: number | null = null
    void action.run(async () => {
      number = (await acceptProposal(p.id)).ticket_number ?? null
    }).then((ok) => {
      if (!ok) return
      if (c.action === 'discard') onDone({ kind: 'discarded' })
      else if (number !== null) onDone({ kind: c.action === 'open_ticket' ? 'opened' : 'attached', ticket: number })
    })
  }

  function discard() {
    void action.run(() => discardProposal(p.id)).then((ok) => { if (ok) onDiscarded() })
  }

  return (
    <div className="inbox-proposal">
      <SuggestionTag addon={p.addon} />
      <ul className="proposal-list">
        <ProposalItem proposal={p} busy={action.busy} onAccept={accept} onDiscard={discard}
          extra={<Problem error={action.error} />} />
      </ul>
    </div>
  )
}
