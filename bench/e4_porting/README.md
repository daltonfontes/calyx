# E4: the bugs, written by someone else

*Em português: [README.pt.md](README.pt.md).*

Thank you for helping. This experiment measures **where a bug in an agent
workflow shows up in Python**: in the type checkers before running, when
running, or nowhere. The same bugs were already written in another
language; for the comparison to be fair, whoever writes them in Python must
be someone else, writing their own way.

## What to do

For each bug in [`bugs.md`](bugs.md), write a program in
`programas/bNN_name.py` (the number with two digits) that:

1. does what the "Program" column describes, **with the mistake** in the
   "The mistake" column;
2. runs on its own, start to finish, with `python programas/bNN_name.py`
   (including the crash and the resume, when the bug has one: simulate the
   crash however feels natural, for example with an exception halfway and a
   second call that resumes from the checkpoint);
3. starts with this header:

   ```python
   # Bug NN: <the sentence from the "The mistake" column>
   # Damage: <where, in the code, the damage happens, if it does>
   ```

**Write it the way you normally would** careful production code:

- with types everywhere (`TypedDict` for the state, annotated functions);
- with LangGraph (`StateGraph`) when there is a flow of steps, and plain
  Python when there is not;
- using the fake world in [`world.py`](world.py) (store, accounts,
  repository, model); extend it if you need to.

Don't try to hide the mistake from the checkers or make it easy for them:
the mistake is one a programmer would make without noticing.

**Don't open**, until you finish: `tests/state_bugs/`, `bench/q2_bugs/`,
`docs/evaluation/`, `docs/paper/` and `paper/`, nor the results sections of
the READMEs. They hold the other versions and the expected results.

If a bug has no natural equivalent in Python, create the file with just the
header and `# Not applicable: <why>`. That is a result too.

**Partial work counts.** All 54 take roughly a day; any subset of 10 or more,
in any order, is already useful. Say which ones you skipped and why in the
notes.

## Set up

```sh
python3 -m venv /tmp/e4 && /tmp/e4/bin/pip install langgraph langgraph-checkpoint-sqlite pyright mypy
```

## Check while you write

```sh
/tmp/e4/bin/python bench/e4_porting/run_e4.py              # all
/tmp/e4/bin/python bench/e4_porting/run_e4.py b13          # just one
```

For each program, the script runs **pyright** and **mypy in strict mode**,
then the program itself, and says what each one found. Don't change the
program to "pass" or "fail": run it only to see that it runs.

## Hand in

Fork the repository, write in `bench/e4_porting/programas/` and open a pull
request with the files in `programas/` and a short note in
`programas/NOTES.md`: how long it took, which bugs were hard to write in
Python and why, and anything odd you noticed. You will be credited in the
paper and in the results, by name or handle, as you prefer.

## How the results are read

For each bug, where it shows up first:

| Where | When |
|---|---|
| **pyright** or **mypy** | The checker reports an error related to the bug |
| **when running, no damage** | The program stops (exception) before the damage |
| **when running, with damage** | The program stops, but the damage already happened |
| **nowhere** | The program finishes normally, with the damage |

The first two rows come from the script. Telling "no damage" from "with
damage", and checking that a checker's error is really about the bug, is
done afterwards by reading the program, by someone who did not write it.
