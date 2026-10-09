# The 54 bugs

*Em português: [bugs.pt.md](bugs.pt.md).*

Each one is an agent workflow with a programming mistake. Write the program
**with the mistake**, in the most natural Python (with LangGraph when there
is a flow of steps), and let the tools find it, if they do. "What to watch"
says what counts as damage.

Some bugs may have no natural equivalent in Python. In that case, write the
closest program you can, or create the file with just the header and
`# Not applicable: <why>`.

## External effects (payment, e-mail)

| # | Program | The mistake | What to watch |
|---|---|---|---|
| 01 | Two parallel steps compute a value and write it to the same state field | The last to finish overwrites the other | One of the values is lost |
| 02 | Writes an e-mail (model) and sends it | The process crashes mid-send; on resume, the program has not decided whether to resend | E-mail duplicated or lost on resume |
| 03 | An agent (model → tools loop) handles a request and has the send-e-mail tool | The agent may call send on every turn | Duplicate e-mails |
| 04 | Makes a refund and sends the confirmation e-mail | The e-mail may go out before the refund, or without it if the refund fails | Confirmation of a refund that did not happen |
| 05 | Before paying, passes the payment service a condition to check ("only if the order was delivered") | The service cannot check conditions and ignores it without an error | Pays even with the condition false |
| 06 | Pays only if a condition on the order holds | The condition uses a misspelled field name | The condition is never true, or crashes |
| 07 | Pays only if a condition on the order holds | The condition compares text with a number | The condition is never true, or crashes |
| 08 | Pays only if a condition holds | The condition calls another tool (an external query) instead of looking at the state read together with the payment | The state changes between the query and the payment |
| 09 | Calls a service that pays and returns the payment id | If the call gets no answer (timeout), the program carries on as if it succeeded and makes up an id | The rest of the flow uses an id that does not exist |
| 10 | Sends an e-mail; if no answer comes, checks whether it went out | The check uses a function that also writes (for example, "check and resend") | The check sends another e-mail |
| 11 | Sends an e-mail; if no answer comes, checks whether it went out | The check looks at something that does not identify this send (for example, "any e-mail to the customer today") | Concludes it went out when it did not, or the reverse |
| 12 | A writing operation is treated as a read (for example, automatic retry as if it were safe) | The write is repeated on failures | Duplicate effect |
| 13 | Pays and, on timeout, tries again | The payment carries no idempotency key | Pays twice |
| 14 | Says the e-mail goes out after the payment | The dependency points to a misnamed step | The intended order does not exist |
| 15 | Two writing steps, ordered | Each waits for the other | The run never finishes |
| 16 | A flow that should only read (so it can run without approval) | It calls a payment | Pays without approval |
| 17 | Makes a refund that may be declined (`None`) | Uses the result as if it succeeded | Carries on (and tells the customer) with a refund that did not happen |
| 18 | Sends an e-mail | The process crashes after sending and before getting the answer; the resume resends | Duplicate e-mail |
| 19 | Decides a refund (model) and pays | The order is cancelled between the decision and the payment | Pays a cancelled order |
| 20 | Checks the order status in one read and pays in a later step | The order may change in between | Pays based on a stale status |
| 21 | Pays with an idempotency key | The key is the order, not the refund request | A second legitimate refund of the same order is silently dropped |

## Files and repository

| # | Program | The mistake | What to watch |
|---|---|---|---|
| 22 | Processes a list of items in parallel, each writing a result | They all write the same file | The result depends on the order |
| 23 | Fixes a repository with several items in parallel (one per file) | They all edit the same repository at the same time | Edits overwrite each other |
| 24 | One step edits the repository while another only reads | The editing step was treated as a read, and both run together | The read sees a half-done edit |
| 25 | Edits the repository and runs the tests | The tests run at the same time as the edit | The test result depends on who gets there first |
| 26 | A step creates a working repository | It is kept in the state and used later by other steps without control | Nobody knows who owns it; two uses get mixed |
| 27 | A function that edits the repository | It is treated as a read; a failure halfway undoes nothing | The edit is left half-done |
| 28 | An agent has the edit-files tool | The path (directory) is an argument the model chooses | The model can write anywhere |
| 29 | Works on a copy of the repository | A function writes to the original directory, not the copy | The copy protects nothing |
| 30 | Edits a file and tries again if it fails | The failure happens after part of the file was written | The retry starts from a half-written file |
| 31 | Makes several edits in sequence, with resume | The process crashes in the middle of an edit | The resume finds the repository in a state the checkpoint does not know |
| 32 | Runs the tests, treated as a read | The tests write files (caches) into the repository | A "read" changed the repository |

## State shared between runs (accounts)

| # | Program | The mistake | What to watch |
|---|---|---|---|
| 33 | Deposits into an account | Reads the balance, adds and writes back; two runs at the same time | A deposit is lost |
| 34 | The function that updates the account | Calls a model while updating | The account stays locked waiting, and the result changes every time |
| 35 | Asks an account operation for an answer | The operation changes the state and returns nothing | The program uses an answer that does not exist |
| 36 | Sends a deposit to the account | The operation's name is misspelled | The deposit vanishes |
| 37 | An account operation changes a field | The field does not exist | The change is lost |
| 38 | Deposits and then checks the balance in the same run | No order between the two | The check may see the old balance |
| 39 | Several runs for the same user deposit at the same time | — | Lost deposits |
| 40 | Deposits, with resume | The process crashes after the deposit is applied and before the run records it; the resume deposits again | Duplicate deposit |
| 41 | Deposits | The user clicked twice: two runs with the same deposit | Duplicate deposit |

## Waiting for people

| # | Program | The mistake | What to watch |
|---|---|---|---|
| 42 | Waits for an approval | No deadline | If nobody answers, it waits forever |
| 43 | Waits for an answer from outside | Waits for a value of a type no external interface can deliver | Nobody can answer |
| 44 | Waits for an approval | The same approval is delivered twice (two clicks) | The second is applied again |
| 45 | Waits for an approval with a 3-day deadline | The machine restarts during the wait and the deadline is counted again | The deadline never expires |
| 54 | Waits for an approval with a deadline, and the process is stopped when the deadline expires | The answer arrives after the deadline, before anyone resumes | The late answer is accepted and the refund goes out |

## Races, rounds and loops

| # | Program | The mistake | What to watch |
|---|---|---|---|
| 46 | Two charging strategies race; the first to finish wins | The loser also charges before the race is decided | The customer is charged twice |
| 47 | Two fixing strategies race | Both edit the same repository at the same time | The edits get mixed |
| 48 | A race between two strategies, with resume | The process crashes after the race is decided; on resume, another one finishes first | The run continues with a different winner than the one it already used |
| 49 | A debate in rounds between agents | One agent reads the current round's answers while the others are still answering | Each run sees a different set |
| 50 | Two strategies race; the fast one wins | The slow one keeps calling the model | The bill grows for nothing |
| 51 | A race with a condition to accept the result | No branch passes, and the program does not say what to do | Carries on with a value that does not exist |
| 52 | A race with a condition to accept the result | The condition calls a model to judge each answer | Each branch makes one more paid call, and the result changes every run |
| 53 | Repeats a task until a check passes | The payment is inside the loop and goes out again on every turn | Pays several times |
