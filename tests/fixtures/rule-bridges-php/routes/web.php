<?php
// Fixture (bridges gate, laravel family): a route through the Route facade.
use App\Http\Controllers\UserController;
use Illuminate\Support\Facades\Route;

Route::get('/laravel/users/{id}', [UserController::class, 'show']);
