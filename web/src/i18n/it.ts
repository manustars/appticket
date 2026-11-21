// Italian. Each message is one literal, as in ./en.ts. Domain words: Ticket, Posta in arrivo,
// Richiedente, Operatore, Cliente, Assegnatario, Nota interna, Risposta, In coda, Inviato; states
// Aperto, In attesa, Risolto, Chiuso.
import type { Catalog, Mismatched, None } from './catalog'

export const it = {
  'app.title': 'Help desk',
  'language.label': 'Lingua',
  'language.name': 'Italiano',

  'error.unreachable': 'Il core non è raggiungibile.',
  'error.loadFailed': 'Impossibile caricare i dati dal core.',

  'signIn.email': 'E-mail',
  'signIn.password': 'Password',
  'signIn.submit': 'Accedi',
  'signIn.wrong': 'E-mail o password errati.',

  'desk.tickets': 'Ticket',
  'desk.inbox': 'Posta in arrivo',
  'desk.signOut': 'Esci',

  'state.open': 'Aperto',
  'state.waiting': 'In attesa',
  'state.resolved': 'Risolto',
  'state.closed': 'Chiuso',

  'customer.helpDesk': '{name} (help desk)',
  'customer.choose': 'Scegliere un cliente…',
  'customer.loading': 'Caricamento dei clienti…',

  'tickets.empty': 'Ancora nessun ticket.',
  'tickets.number': 'N.',
  'tickets.title': 'Titolo',
  'tickets.customer': 'Cliente',
  'tickets.requester': 'Richiedente',
  'tickets.assignee': 'Assegnatario',
  'tickets.state': 'Stato',
  'tickets.updated': 'Aggiornato',

  'ticket.back': '← Ticket',
  'ticket.customer': 'Cliente',
  'ticket.changeCustomer': 'Cambia',
  'ticket.requester': 'Richiedente',
  'ticket.noRequester': 'Nessuno',
  'ticket.state': 'Stato',
  'ticket.assignee': 'Assegnatario',
  'ticket.unassigned': 'Non assegnato',
  'ticket.opened': 'Aperto il',
  'ticket.confirmClose':
    'Chiudere il ticket #{number}? La chiusura è definitiva: il ticket non potrà essere riaperto e un nuovo messaggio del richiedente aprirà un nuovo ticket.',
  'ticket.moved': 'Ticket spostato.',
  'ticket.movedWithRequester': {
    one: 'Spostato {count} ticket di questo richiedente.',
    other: 'Spostati {count} ticket di questo richiedente.',
  },
  'ticket.emptyTimeline': 'Su questo ticket non è ancora stato scritto nulla.',
  'ticket.closedNotice':
    'Questo ticket è chiuso in modo definitivo. Un nuovo messaggio del richiedente apre un nuovo ticket. Gli operatori possono comunque aggiungere note interne.',

  'entry.note': 'Nota interna',
  'entry.outgoing': 'Messaggio in uscita',
  'entry.incoming': 'Messaggio in arrivo',
  'entry.to': 'a {address}',
  'entry.sent': 'Inviato il {time}',
  'entry.notSent': 'Non inviato',
  'entry.queued': 'In coda',
  'entry.gaveUp':
    "Questo messaggio non è stato inviato: l'help desk ha smesso di riprovare. Ultimo errore: {error}",
  'entry.unknownError': 'sconosciuto',
  'entry.retrying': 'Non ancora inviato, verrà ritentato: {error}',

  'composer.modes': 'Tipo di testo',
  'composer.reply': 'Rispondi al richiedente',
  'composer.note': 'Nota interna',
  'composer.hintReply': 'Inviata per e-mail a <strong>{email}</strong>.',
  'composer.hintClosed':
    'Il ticket è chiuso e non riceve altre risposte. Le note sono visibili solo agli operatori.',
  'composer.hintNote':
    'Le note sono visibili solo agli operatori e <strong>non vengono mai inviate</strong> al richiedente.',
  'composer.hintNoRequester':
    "Questo ticket non ha un richiedente, quindi non c'è nessuno a cui rispondere. Le note sono visibili solo agli operatori.",
  'composer.replyLabel': 'Risposta al richiedente',
  'composer.noteLabel': 'Nota interna',
  'composer.replyPlaceholder': 'Scrivi al richiedente…',
  'composer.notePlaceholder': 'Scrivi una nota per gli operatori…',
  'composer.then': "Stato del ticket dopo l'invio",
  'composer.sendReply': 'Invia la risposta a {email}',
  'composer.addNote': 'Aggiungi nota interna',

  'move.to': 'Spostare questo ticket al cliente',
  'move.requesterBelongs': 'Il richiedente {email} appartiene a {customer}.',
  'move.requesterToo': 'Il richiedente era associato al cliente sbagliato: spostare anche lui',
  'move.requesterTooWithTickets': {
    one: 'Il richiedente era associato al cliente sbagliato: spostare anche lui, con il suo altro ticket',
    other:
      'Il richiedente era associato al cliente sbagliato: spostare anche lui, con i suoi altri {count} ticket',
  },
  'move.requesterStays': 'Lasciare il richiedente presso {customer} e toglierlo da questo ticket',
  'move.cancel': 'Annulla',
  'move.submit': 'Sposta il ticket',

  'inbox.empty': 'La posta in arrivo è vuota.',
  'inbox.noSubject': '(senza oggetto)',
  'inbox.goToTicket': 'Vai al ticket #{number}',
  'inbox.opened': 'Ticket #{number} aperto da “{subject}”.',
  'inbox.attached': 'Messaggio agganciato al ticket #{number}.',
  'inbox.discarded': 'Messaggio di {sender} scartato.',
  'inbox.confirmDiscard': 'Scartare il messaggio di {sender}? Non entrerà a far parte di alcun ticket.',
  'inbox.notATicket': 'Inserire il numero di un ticket esistente.',
  'inbox.open': 'Apri un ticket',
  'inbox.attach': 'Aggancia a un ticket',
  'inbox.discard': 'Scarta',
  'inbox.customer': 'Cliente',
  'inbox.openSubmit': 'Apri il ticket',
  'inbox.ticketNumber': 'Numero del ticket',
  'inbox.attachSubmit': 'Aggancia',
  'inbox.cancel': 'Annulla',
} as const satisfies Catalog

// Fails to compile, naming the keys, when a message has other placeholders or tags than English.
export type ItalianPlaceholders = None<Mismatched<typeof it>>
