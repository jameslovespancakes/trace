<?php
// Fixture (bridges gate, laravel family): the controller action.
namespace App\Http\Controllers;

class UserController
{
    public function show($id)
    {
        return $id;
    }
}
