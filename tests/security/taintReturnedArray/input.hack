function processParams(dict<arraykey, mixed> $params) : dict<arraykey, mixed> {
    if (isset($params["foo"])) {
        return $params;
    }

    return dict[];
}

$params = processParams($_GET);

echo $params["foo"];