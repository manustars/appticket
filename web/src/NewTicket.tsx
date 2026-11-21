import { useId, useState, type FormEvent } from 'react'
import { listCustomers, openTicket } from './api/client'
import { useI18n } from './i18n/context'
import { Problem } from './Problem'
import { go, ticketHref } from './route'
import { useAction, useLoad } from './useAction'

const loadCustomers = () => listCustomers()

interface Props {
  /** The customer to start from, if any; ignored unless it can get tickets. */
  customerId: string
  onCancel: () => void
  onSignedOut: () => void
}

/** Opens a ticket by hand, e.g. after a phone call, then shows it. */
export function NewTicket({ customerId: initial, onCancel, onSignedOut }: Props) {
  const { t } = useI18n()
  const customers = useLoad(loadCustomers, onSignedOut)
  const action = useAction(onSignedOut)
  const [chosen, setChosen] = useState(initial)
  const [title, setTitle] = useState('')
  const [email, setEmail] = useState('')
  const [note, setNote] = useState('')
  const id = useId()
  // Only a customer that gets tickets can be chosen; an offboarded one from the filter is dropped.
  const customerId = customers.data?.some((c) => c.id === chosen) ? chosen : ''

  function submit(event: FormEvent) {
    event.preventDefault()
    if (!customerId || !title.trim()) return
    let number = 0
    void action.run(async () => {
      number = await openTicket({
        customer_id: customerId,
        title: title.trim(),
        requester_email: email.trim() || null,
        note: note.trim() || null,
      })
    }).then((ok) => { if (ok) go(ticketHref(number)) })
  }

  return (
    <section className="panel new-ticket-form" aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`}>{t('tickets.new')}</h2>
      <form className="stacked" onSubmit={submit}>
        <div className="inline-form">
          <label>
            {t('newTicket.customer')}
            <select required value={customerId} disabled={!customers.data}
              onChange={(e) => setChosen(e.target.value)}>
              <option value="">{customers.data ? t('customer.choose') : t('customer.loading')}</option>
              {customers.data?.map((c) => (
                <option key={c.id} value={c.id}>{c.is_help_desk ? t('customer.helpDesk', { name: c.name }) : c.name}</option>
              ))}
            </select>
          </label>
          <label className="wide">
            {t('newTicket.title')}
            <input required value={title} autoFocus onChange={(e) => setTitle(e.target.value)} />
          </label>
        </div>
        <label>
          {t('newTicket.requester')}
          <input type="email" value={email} aria-describedby={`${id}-requester`}
            onChange={(e) => setEmail(e.target.value)} />
        </label>
        <p className="hint" id={`${id}-requester`}>{t('newTicket.requesterHint')}</p>
        <label>
          {t('newTicket.note')}
          <textarea rows={4} value={note} placeholder={t('newTicket.notePlaceholder')} aria-describedby={`${id}-note`}
            onChange={(e) => setNote(e.target.value)} />
        </label>
        <p className="hint" id={`${id}-note`}>{t('newTicket.noteHint')}</p>
        <Problem error={customers.error ?? action.error} />
        <div className="move-foot">
          <button type="button" onClick={onCancel}>{t('action.cancel')}</button>
          <button type="submit" disabled={action.busy || !customerId || !title.trim()}>{t('newTicket.submit')}</button>
        </div>
      </form>
    </section>
  )
}
