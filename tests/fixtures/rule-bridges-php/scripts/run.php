<?php
// Fixture (bridges gate, php subprocess family): starts a repository script.
function build()
{
    return exec('php scripts/job.php');
}
