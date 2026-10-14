<?php
namespace App\Services;

use App\Models\User;
use App\Support\Helpers as H;
use function strlen;

interface Contract { public function run(): void; }

trait Loggable { public function log($m) { error_log($m); } }

enum Status { case Active; }

class Service implements Contract {
    use Loggable;
    public function run(): void {
        $u = new User();
        $u->save();
        self::helper();
        H::format(strlen("x"));
        \App\Other::call();
        $fn = $this->get();
        $fn();
    }
    private static function helper() { return 1; }
}

function topLevel() { return Service::class; }
