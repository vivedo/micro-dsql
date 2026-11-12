// Driver smoke test: node-postgres (pg). Settings from PG* env vars; params are sent as text.
const { Client } = require('pg');
const NAME = 'node-pg';
let failures = 0;

async function step(label, fn) {
  try {
    const res = await Promise.race([
      fn(),
      new Promise((_, rej) => setTimeout(() => rej(new Error('TIMEOUT after 10s')), 10000)),
    ]);
    console.log(`[${NAME}] ${label}: OK ${res === undefined ? '' : JSON.stringify(res)}`.trimEnd());
  } catch (e) {
    failures++;
    console.log(`[${NAME}] ${label}: ERR ${e.code || 'client'} ${String(e.message).split('\n')[0]}`);
  }
}

(async () => {
  console.log(`[${NAME}] version pg ${require('pg/package.json').version}, node ${process.version}`);
  const c = new Client({ connectionTimeoutMillis: 5000, ssl: false });
  try {
    await c.connect();
  } catch (e) {
    console.log(`[${NAME}] connect: ERR ${e.message}`);
    process.exit(1);
  }
  console.log(`[${NAME}] connect: OK`);
  const ts = new Date('2026-11-12T10:00:00.123Z');
  const u = '11111111-1111-1111-1111-111111111111';
  await step('drop', async () => { await c.query('DROP TABLE IF EXISTS drv_node'); });
  await step('create', async () => {
    await c.query('CREATE TABLE drv_node (id bigint PRIMARY KEY, name text NOT NULL, n int, amount numeric(10,2), ok bool, at timestamptz, u uuid, doc jsonb)');
  });
  await step('insert params', async () => (await c.query(
    'INSERT INTO drv_node VALUES ($1, $2, $3, $4, $5, $6, $7, $8)', [1, 'one', 1, '1.50', true, ts, u, { a: 1 }])).rowCount);
  await step('select by param', async () => (await c.query('SELECT * FROM drv_node WHERE id = $1', [1])).rows[0]);
  await step('timestamptz round trip', async () => {
    const r = (await c.query('SELECT at FROM drv_node WHERE id = $1', [1])).rows[0].at;
    return r instanceof Date && r.getTime() === ts.getTime() ? 'exact' : `DIFF ${r}`;
  });
  await step('update by param', async () => (await c.query('UPDATE drv_node SET name = $1, n = n + 1 WHERE id = $2', ['uno', 1])).rowCount);
  await step('explicit txn commit', async () => {
    await c.query('BEGIN');
    await c.query('INSERT INTO drv_node (id, name) VALUES ($1, $2)', [2, 'two']);
    await c.query('UPDATE drv_node SET n = $1 WHERE id = $2', [20, 2]);
    await c.query('COMMIT');
    return (await c.query('SELECT n FROM drv_node WHERE id = 2')).rows[0];
  });
  await step('explicit txn rollback', async () => {
    await c.query('BEGIN');
    await c.query('INSERT INTO drv_node (id, name) VALUES ($1, $2)', [3, 'three']);
    await c.query('ROLLBACK');
    return (await c.query('SELECT count(*)::int AS n FROM drv_node WHERE id = 3')).rows[0];
  });
  await step('named prepared statement', async () => {
    const q = { name: 'by_id', text: 'SELECT name FROM drv_node WHERE id = $1' };
    const a = await c.query({ ...q, values: [1] });
    const b = await c.query({ ...q, values: [2] });
    return [a.rows[0], b.rows[0]];
  });
  await step('delete by param', async () => (await c.query('DELETE FROM drv_node WHERE id = $1', [1])).rowCount);
  await step('count', async () => (await c.query('SELECT count(*) FROM drv_node')).rows[0]);
  await step('error SQLSTATE (dup key)', async () => { await c.query("INSERT INTO drv_node (id, name) VALUES (2, 'dup')"); });
  await c.end();
  console.log(`[${NAME}] failures=${failures}`);
})();
