<?php
class Service {
    /** @var Client|null */
    private $client;

    /**
     * @param Request $req
     * @return static
     */
    public function handle($req) {
        /** @var Parser $p */
        $p = make();
        $s = '/** @var Nope $q */';
        return $this;
    }
}
