// Fixture (bridges gate, java client families: JDK HttpClient, okhttp, RestTemplate).
package demo;

import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;

import okhttp3.OkHttpClient;
import okhttp3.Request;

public class Clients {
    public String jdk() throws Exception {
        HttpRequest request = HttpRequest.newBuilder(URI.create("http://svc.local/spring/users/1")).build();
        return HttpClient.newHttpClient().send(request, HttpResponse.BodyHandlers.ofString()).body();
    }

    public void ok() throws Exception {
        Request request = new Request.Builder().url("http://svc.local/jaxrs/items/2").build();
        new OkHttpClient().newCall(request).execute();
    }
}
