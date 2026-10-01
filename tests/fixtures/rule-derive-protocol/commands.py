"""Fixture (rule: protocol command tokens). A client sends broker commands as the token
followed by its arguments; the io_send row names the token and the key offset."""


class Commands:
    def publish(self, channel, message, **kwargs):
        return self.execute_command("PUBLISH", channel, message, **kwargs)

    def shard_publish(self, shard, message):
        return self.execute_command("spublish", shard, message)

    def fixed(self, message):
        return self.execute_command("PUBLISH", "alerts", message)

    def read(self, name):
        return self.execute_command("GET", name)


class Facade(Commands):
    def notify(self, topic, text):
        return self.publish(topic, text)
