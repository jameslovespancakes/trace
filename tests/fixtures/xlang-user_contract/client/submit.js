// Fixture (P5, user_contract): a JavaScript producer; the manifest links it to service/handler.py.
function submit_job(payload) {
  QUEUE.push(payload);
}

module.exports = { submit_job };
