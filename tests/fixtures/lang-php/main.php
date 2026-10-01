<?php
namespace App;

require_once __DIR__ . '/Util.php';

function run(): string
{
    return Util::greet("trace");
}
