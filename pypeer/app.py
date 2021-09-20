import logging
import sys

from PySide2.QtMultimedia import QAudioFormat, QAudioDeviceInfo, QAudioInput
from PySide2.QtWidgets import QMainWindow, QApplication
from PySide2.QtNetwork import QUdpSocket

from audio import PeerAudioPlayer


class MainWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        self._player = PeerAudioPlayer(self)

        format = QAudioFormat()
        format.setSampleRate(44100)
        format.setChannelCount(2)
        format.setSampleSize(32)
        format.setCodec("audio/pcm")
        format.setByteOrder(QAudioFormat.LittleEndian)
        format.setSampleType(QAudioFormat.Float)

        info = QAudioDeviceInfo(QAudioDeviceInfo.defaultInputDevice())
        if not info.isFormatSupported(format):
            format = info.nearestFormat(format)
        logging.info(
            f"Using audio input device `{info.deviceName()}` with format `{format}`"
        )

        # Open a UDP socket and send all audio input to 127.0.0.1:9898
        input = QAudioInput(format)
        self._socket = QUdpSocket()
        self._socket.connectToHost("127.0.0.1", 9898)
        self._device = input.start(self._socket)

        # Log the input buffer size used, note that the actual buffer size is unknown until `start()` is called
        logging.debug(f"Using audio input buffer size of {input.bufferSize()} bytes")


if __name__ == "__main__":
    # Configure logging to display time in messages and include debug logs
    logging.basicConfig(
        format="%(asctime)s %(levelname)s %(message)s", level=logging.DEBUG
    )

    app = QApplication(sys.argv)

    window = MainWindow()
    window.show()

    sys.exit(app.exec_())
