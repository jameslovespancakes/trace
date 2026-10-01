<?php

function each_item($items, $fn)
{
    foreach ($items as $x) {
        call_user_func($fn, $x);
    }
}

function apply_now($fn)
{
    return $fn(1);
}
